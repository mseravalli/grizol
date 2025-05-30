use crate::core::bep_state::BepState;
use crate::core::{GrizolConfig, GrizolFileInfo, GrizolFolder};
use crate::storage::StorageManager;
use crate::syncthing::Folder;
use chrono::prelude::*;
use chrono_timesource::TimeSource;
use fuser::{
    FileAttr, FileType, Filesystem, MountOption, ReplyAttr, ReplyData, ReplyEmpty, Request,
    FUSE_ROOT_ID,
};
use libc::{EFAULT, EFBIG, EIO, EISDIR, ENOENT, EPERM};
use std::cell::Ref;
use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::ffi::OsStr;
use std::ops::Deref;
use std::path::Path;
use std::rc::{Rc, Weak};
use std::sync::Arc;
use std::time::{Duration, Instant, UNIX_EPOCH};
use tokio::runtime::Runtime;
use tokio::sync::Mutex;

#[derive(Debug)]
enum GrizolFSErr {
    NoParent(GrizolFileInfo),
}

#[derive(Debug, Clone)]
enum GrizolFSFileType {
    File(GrizolFileInfo),
    Folder(GrizolFolder),
}

// We allow 2^32 - 10 operations to be performed on the top dirs.
const ENTRIES_START: u64 = 1 << 32;
const fn entry_id(e_id: i64) -> u64 {
    e_id as u64 + ENTRIES_START
}

const TOP_DIRS_START: u64 = 10;
const fn top_dir_id(d_id: i64) -> u64 {
    let res = d_id as u64 + TOP_DIRS_START;
    if res >= ENTRIES_START {
        panic!("Too many operations performed on the top level dirs");
    }
    res
}

type WeakFsNode = Weak<RefCell<FsNode>>;
type RcFsNode = Rc<RefCell<FsNode>>;

#[derive(Debug)]
struct FsNode {
    id: u64,
    file_type: GrizolFSFileType,
    _parent: Option<WeakFsNode>,
    children: HashMap<String, RcFsNode>,
}

struct GrizolFsMem {
    root: RcFsNode,
    nodes: HashMap<u64, WeakFsNode>,
}

#[derive(Default)]
struct Collector {
    vec: Vec<WeakFsNode>,
}

impl Collector {
    fn collect(&mut self, node: WeakFsNode) -> Result<(), String> {
        self.vec.push(node);
        Ok(())
    }
}

impl GrizolFsMem {
    fn new() -> Self {
        let folder = Folder::default();
        let g_folder = GrizolFolder { folder, id: 1 };

        let root_node = FsNode {
            id: 1,
            file_type: GrizolFSFileType::Folder(g_folder),
            _parent: None,
            children: Default::default(),
        };

        let root = Rc::new(RefCell::new(root_node));
        let mut nodes: HashMap<u64, WeakFsNode> = Default::default();
        nodes.insert(1, Rc::downgrade(&root));

        GrizolFsMem { root, nodes }
    }

    /// Insert top folder
    pub fn insert_folder(&mut self, folder: GrizolFolder) {
        let root = self.root();
        let folder_id = top_dir_id(folder.id);

        let node = FsNode {
            id: folder_id,
            file_type: GrizolFSFileType::Folder(folder.clone()),
            _parent: Some(Rc::downgrade(&root)),
            children: Default::default(),
        };

        let ref_node = Rc::new(RefCell::new(node));
        root.borrow_mut()
            .children
            .insert(folder.folder.id, ref_node.clone());

        self.nodes.insert(folder_id, Rc::downgrade(&ref_node));
    }

    pub fn insert_file(&mut self, file: GrizolFileInfo) -> Result<(), GrizolFSErr> {
        let split_path: Vec<&str> = vec![file.folder.as_str()]
            .into_iter()
            .chain(file.file_info.name.split("/"))
            .collect();

        let mut parent_node = self.root();
        for node_name in split_path.iter().take(split_path.len() - 1) {
            let node = if let Some(n) = parent_node.deref().borrow().children.get(*node_name) {
                n.clone()
            } else {
                return Err(GrizolFSErr::NoParent(file));
            };
            parent_node = node;
        }

        let node_id = entry_id(file.id);
        // Safe to unwrap as there are at least 2 elements
        let node_name = split_path.last().unwrap().to_string();
        let node = FsNode {
            id: node_id,
            file_type: GrizolFSFileType::File(file),
            _parent: Some(Rc::downgrade(&parent_node)),
            children: Default::default(),
        };

        let ref_node = Rc::new(RefCell::new(node));

        parent_node
            .borrow_mut()
            .children
            .insert(node_name, ref_node.clone());

        self.nodes.insert(node_id, Rc::downgrade(&ref_node));

        Ok(())
    }

    // async fn _insert_node(&mut self, _parent_id: u64, _node: FsNode) -> Result<(), String> {
    //     todo!("see if we can merge file and folder");
    // }

    fn root(&self) -> RcFsNode {
        self.root.clone()
    }

    // TODO: find a better way to do this, e.g. an iterator or something.
    // Traverse the file system from the given node in post order fashion and collects the elements.
    fn traverse_post(node: &RcFsNode, mut collector: &mut Collector) -> Result<(), String> {
        for (_, child) in node.deref().borrow().children.iter() {
            Self::traverse_post(child, &mut collector)?;
        }

        collector.collect(Rc::<RefCell<FsNode>>::downgrade(node))
    }

    fn _folder_from_ino(&self, ino: u64) -> Option<String> {
        let mut folder: Option<String> = None;

        let mut traverse_ino = ino;

        // Traverse the tree from the current file upwards until we find a folder.
        // We track the ino to not to have to deal with the references.
        while folder.is_none() {
            let parent = self
                .nodes
                .get(&traverse_ino)
                .unwrap()
                .upgrade()
                .unwrap()
                .deref()
                .borrow()
                ._parent
                .as_ref()
                .unwrap()
                .upgrade()
                .unwrap();
            traverse_ino = parent.deref().borrow().id;
            folder = match &parent.deref().borrow().file_type {
                GrizolFSFileType::Folder(f) => Some(f.folder.id.clone()),
                _ => None,
            };
        }
        folder
    }
}

pub struct GrizolFS<TS: TimeSource<Utc>> {
    state: Arc<Mutex<BepState<TS>>>,
    config: GrizolConfig,
    // TODO: see if one of the other solutions is better in this case: https://tokio.rs/tokio/topics/bridging
    // Runtime is used to allow running async functions.
    rt: Runtime,
    last_folders_files: RefCell<Instant>,
    folders: RefCell<Vec<GrizolFolder>>,
    files: RefCell<Vec<GrizolFileInfo>>,
    storage_manager: StorageManager,
    fs_mem: RefCell<GrizolFsMem>,
}

impl<TS: TimeSource<Utc>> GrizolFS<TS> {
    pub fn new(config: GrizolConfig, state: Arc<Mutex<BepState<TS>>>) -> Self {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();

        GrizolFS {
            storage_manager: StorageManager::new(config.clone()),
            config,
            state,
            rt,
            // We set the instant to a point in time in the past to guarantee that the first time
            // this will be called it will be refreshed. Maybe there is a better way.
            last_folders_files: RefCell::new(
                Instant::now().checked_sub(Duration::from_secs(10)).unwrap(),
            ),
            folders: Default::default(),
            files: Default::default(),
            fs_mem: RefCell::new(GrizolFsMem::new()),
        }
    }

    /// Adds the data from the database to the in memory datastructure.
    fn refresh_expired_folders_files(&self) {
        if self.last_folders_files.borrow().elapsed() >= self.config.fuse_refresh_rate {
            let (folders, files) = self.rt.block_on(async {
                let state = self.state.lock().await;

                let folders = state.top_folders_fuse(self.config.local_device_id).await;
                let files = state.files_fuse(self.config.local_device_id).await;
                (folders, files)
            });

            self.folders.replace(folders);
            self.files.replace(files);
            self.last_folders_files.replace(Instant::now());

            for folder in self.folders.borrow().clone().into_iter() {
                self.fs_mem.borrow_mut().insert_folder(folder);
            }

            let mut files: VecDeque<GrizolFileInfo> =
                self.files.borrow().clone().into_iter().collect();

            // TODO: this can potentially loop forever if for some reason a parent is not present.
            // Add a way to log an error and continue the operations without looping forever, e.g.
            // we will perform at most n^2 operations (ARGHH) if we reach that continue.
            // TODO: to avoid O(n^2) we might sort by number of '/' in the filename so that this
            // operation will be done in linear time?
            while !files.is_empty() {
                let file = if let Some(f) = files.pop_front() {
                    f
                } else {
                    break;
                };
                match self.fs_mem.borrow_mut().insert_file(file) {
                    Ok(_) => {}
                    Err(GrizolFSErr::NoParent(f)) => {
                        files.push_back(f);
                    }
                }
            }
        }
    }

    fn folders_files(&self) -> (Ref<'_, Vec<GrizolFolder>>, Ref<'_, Vec<GrizolFileInfo>>) {
        self.refresh_expired_folders_files();
        (self.folders.borrow(), self.files.borrow())
    }

    fn root_dir_attr(&self) -> FileAttr {
        FileAttr {
            ino: FUSE_ROOT_ID,
            size: 0,
            blocks: 0,
            atime: UNIX_EPOCH,
            mtime: UNIX_EPOCH,
            ctime: UNIX_EPOCH,
            crtime: UNIX_EPOCH,
            kind: FileType::Directory,
            perm: 0o755,
            nlink: 0,
            uid: self.config.uid,
            gid: self.config.gid,
            rdev: 0,
            flags: 0,
            blksize: 512,
        }
    }

    fn attr_from_folder(&self, g_folder: &GrizolFolder) -> FileAttr {
        let modified_ts = UNIX_EPOCH;
        FileAttr {
            ino: top_dir_id(g_folder.id),
            size: 0,
            blocks: 0,
            atime: modified_ts,
            mtime: modified_ts,
            ctime: modified_ts,
            crtime: modified_ts,
            kind: FileType::Directory,
            perm: 0o755,
            nlink: 0,
            uid: self.config.uid,
            gid: self.config.gid,
            rdev: 0,
            flags: 0,
            blksize: 1 << 10,
        }
    }

    fn attr_from_file(&self, g_file: &GrizolFileInfo) -> FileAttr {
        let file = &g_file.file_info;
        let modified_ts = UNIX_EPOCH
            .checked_add(Duration::from_secs(file.modified_s as u64))
            .and_then(|ts| ts.checked_add(Duration::from_nanos(file.modified_ns as u64)))
            .unwrap();
        let res = FileAttr {
            ino: entry_id(g_file.id),
            size: file.size as u64,
            blocks: 0,
            atime: modified_ts,
            mtime: modified_ts,
            ctime: modified_ts,
            crtime: modified_ts,
            kind: file_type(file.r#type),
            perm: file.permissions as u16,
            nlink: 0,
            uid: self.config.uid,
            gid: self.config.gid,
            rdev: 0,
            flags: 0,
            blksize: file.block_size as u32,
        };
        trace!("attr: {:?}", res);
        res
    }

    fn readdir_mem(
        &mut self,
        _req: &Request<'_>,
        ino: u64,
        _fh: u64,
        offset: i64,
        mut reply: fuser::ReplyDirectory,
    ) {
        // This is needed to populate the cache
        // TODO: improve
        let (_folders, _files) = self.folders_files();
        let offset = offset as usize;

        let dir = if let Some(d) = self.fs_mem.borrow().nodes.get(&ino) {
            d.clone()
        } else {
            reply.error(ENOENT);
            return;
        };

        // Safe to unwrap, the reference must always be valid.
        let binding = dir.upgrade().unwrap();
        let children = &binding.deref().borrow().children;

        for (i, (name, node)) in children.iter().enumerate().skip(offset) {
            let file_type = match &node.deref().borrow().file_type {
                GrizolFSFileType::Folder(_) => FileType::Directory,
                GrizolFSFileType::File(f) => file_type(f.file_info.r#type),
            };
            let is_buffer_full =
                reply.add(node.deref().borrow().id, (i + 1) as i64, file_type, name);
            if is_buffer_full {
                trace!("The buffer is full");
                reply.ok();
                return;
            }
        }
        reply.ok();
    }

    fn readdir_db(
        &mut self,
        _req: &Request<'_>,
        ino: u64,
        _fh: u64,
        offset: i64,
        mut reply: fuser::ReplyDirectory,
    ) {
        trace!("Start reading dir (from db) with ino {}", ino);
        let offset = offset as usize;

        let (folders, files) = self.folders_files();

        if ino == FUSE_ROOT_ID {
            if offset == folders.len() {
                reply.ok();
                return;
            }

            for (i, el) in folders.iter().enumerate().skip(offset) {
                let folder = &el.folder;

                let file_name = Path::new(&folder.id).as_os_str();

                let is_buffer_full = reply.add(
                    top_dir_id(el.id),
                    (i + 1) as i64,
                    FileType::Directory,
                    file_name,
                );

                if is_buffer_full {
                    trace!("The buffer is full");
                    reply.ok();
                    return;
                }
            }

            reply.ok();
            return;
        }

        let top_folder = top_folder(ino, &folders, &files);
        let base_dir = base_dir(ino, &files);

        if top_folder.as_ref().and(base_dir.as_ref()).is_none() {
            reply.error(ENOENT);
            return;
        }

        let top_folder = top_folder.unwrap();
        let base_dir = base_dir.unwrap();

        let files: Vec<&GrizolFileInfo> = files
            .iter()
            .filter(|file| file.folder == top_folder && is_file_in_current_dir(&base_dir, file))
            .collect();

        if offset == files.len() {
            reply.ok();
            return;
        }

        for (i, el) in files.iter().enumerate().skip(offset) {
            let file = &el;

            let file_name = Path::new(&file.file_info.name)
                .strip_prefix(Path::new(&base_dir))
                .unwrap()
                .as_os_str();

            let is_buffer_full = reply.add(
                entry_id(file.id),
                (i + 1) as i64,
                file_type(file.file_info.r#type),
                file_name,
            );

            if is_buffer_full {
                debug!("The buffer is full");
                reply.ok();
                return;
            }
        }

        reply.ok();
    }

    fn lookup_mem(
        &mut self,
        _req: &Request<'_>,
        parent_ino: u64,
        name: &std::ffi::OsStr,
        reply: fuser::ReplyEntry,
    ) {
        // This is needed to populate the cache
        // TODO: improve
        let (_folders, _files) = self.folders_files();

        let name = if let Some(n) = name.to_str() {
            n
        } else {
            reply.error(EFAULT);
            return;
        };

        if let Some(parent) = self.fs_mem.borrow().nodes.get(&parent_ino) {
            // Safe to unwrap, the reference must always be valid.
            if let Some(node) = parent
                .upgrade()
                .unwrap()
                .deref()
                .borrow()
                .children
                .get(name)
            {
                let attribute = match &node.deref().borrow().file_type {
                    GrizolFSFileType::Folder(f) => self.attr_from_folder(&f),
                    GrizolFSFileType::File(f) => self.attr_from_file(&f),
                };

                reply.entry(&self.config.fuse_refresh_rate, &attribute, 1);
                return;
            }
        }

        reply.error(ENOENT);
    }

    fn lookup_db(
        &mut self,
        _req: &Request<'_>,
        parent_ino: u64,
        name: &std::ffi::OsStr,
        reply: fuser::ReplyEntry,
    ) {
        let (folders, files) = self.folders_files();

        let attr = if parent_ino == FUSE_ROOT_ID {
            folders
                .iter()
                .find(|f| f.folder.id == name.to_str().unwrap())
                .map(|f| self.attr_from_folder(f))
        } else {
            let parent_dir = parent_dir(&files, parent_ino);

            files
                .iter()
                .filter(|f| {
                    Path::new(&f.file_info.name)
                        .file_name()
                        .map(|f| f == name)
                        .unwrap_or(false)
                })
                .find(|&f| is_file_in_current_dir(&parent_dir, f))
                .map(|f| self.attr_from_file(f))
        };

        if let Some(a) = attr {
            reply.entry(&self.config.fuse_refresh_rate, &a, 1);
        } else {
            reply.error(ENOENT);
        }
    }

    fn getattr_mem(&mut self, _req: &Request, ino: u64, reply: ReplyAttr) {
        // This is needed to populate the cache
        // TODO: improve
        let (_folders, _files) = self.folders_files();

        if ino == FUSE_ROOT_ID {
            reply.attr(&self.config.fuse_refresh_rate, &self.root_dir_attr());
            return;
        }

        if let Some(node) = self.fs_mem.borrow().nodes.get(&ino) {
            let n = node.upgrade().unwrap();
            let attr = match &n.deref().borrow().file_type {
                GrizolFSFileType::Folder(f) => self.attr_from_folder(&f),
                GrizolFSFileType::File(f) => self.attr_from_file(&f),
            };

            trace!("Adding attr: {:?}", &attr);
            reply.attr(&self.config.fuse_refresh_rate, &attr);
            return;
        }

        reply.error(ENOENT);
    }

    fn getattr_db(&mut self, _req: &Request, ino: u64, reply: ReplyAttr) {
        if ino == FUSE_ROOT_ID {
            reply.attr(&self.config.fuse_refresh_rate, &self.root_dir_attr());
            return;
        }

        let (folders, files) = self.folders_files();

        let attr = if ino < ENTRIES_START {
            folders
                .iter()
                .find(|&f| top_dir_id(f.id) == ino)
                .map(|f| self.attr_from_folder(f))
        } else {
            files
                .iter()
                .find(|&f| entry_id(f.id) == ino)
                .map(|f| self.attr_from_file(f))
        };

        debug!("Adding attr: {:?}", &attr);
        if let Some(a) = attr {
            reply.attr(&self.config.fuse_refresh_rate, &a);
        } else {
            reply.error(ENOENT);
        }
    }

    async fn read_from_local_cache(
        &self,
        file: &GrizolFileInfo,
        offset: i64,
        size: u32,
    ) -> Result<Vec<u8>, String> {
        let folder = &file.folder;
        let file_name = &file.file_info.name;

        let state = self.state.lock().await;
        if !state
            .is_file_in_read_cache(&self.config.local_device_id, &folder, &file_name)
            .await
        {
            let removed_files = state
                .free_read_cache(
                    self.config.read_cache_size,
                    file.file_info.size.try_into().unwrap(),
                )
                .await
                .expect("Failed to remove from database");
            debug!("removed_files: {:?}", removed_files);
            self.storage_manager
                .free_read_cache(&removed_files)
                .await
                .map_err(|e| {
                    format!(
                        "Could not remove '{:?}' from the temporary storage due to: {}",
                        removed_files, e
                    )
                })?;
            let _path_in_cache = self
                .storage_manager
                .cp_remote_to_read_cache(&file, &file.file_locations)
                .await?;
        }

        state
            .update_read_cache(&self.config.local_device_id, &folder, &file_name)
            .await;
        self.storage_manager.read_cached(&file, offset, size).await
    }

    fn read_mem(
        &mut self,
        _req: &Request<'_>,
        ino: u64,
        _fh: u64,
        offset: i64,
        size: u32,
        _flags: i32,
        _lock_owner: Option<u64>,
        reply: ReplyData,
    ) {
        // This is needed to populate the cache
        // TODO: improve
        let (_folders, _files) = self.folders_files();

        if ino < ENTRIES_START {
            reply.error(EISDIR);
            return;
        };

        let node = if let Some(n) = self.fs_mem.borrow().nodes.get(&ino) {
            n.upgrade().unwrap()
        } else {
            reply.error(ENOENT);
            return;
        };
        let node = node.deref().borrow();

        let file = match &node.file_type {
            GrizolFSFileType::Folder(_) => {
                reply.error(EISDIR);
                return;
            }
            GrizolFSFileType::File(f) => f,
        };

        if u64::try_from(file.file_info.size).unwrap() > self.config.read_cache_size {
            reply.error(EFBIG);
            return;
        }

        trace!("file to be read: {:?}", file);

        let file_name = &file.file_info.name;

        let data = self
            .rt
            .block_on(async { self.read_from_local_cache(file, offset, size).await });

        match data {
            Ok(d) => reply.data(&d),
            Err(e) => {
                error!("Could not read '{}' due to: {}", file_name, e);
                reply.error(EIO)
            }
        }
    }

    fn rename_mem(
        &mut self,
        _req: &Request<'_>,
        orig_parent_dir_ino: u64,
        orig_name: &OsStr,
        dest_parent_dir_ino: u64,
        dest_name: &OsStr,
        _flags: u32,
        reply: ReplyEmpty,
    ) {
        warn!(
            "Start moving {} {:?} to {} {:?}",
            &orig_parent_dir_ino, orig_name, &dest_parent_dir_ino, dest_name
        );

        if orig_parent_dir_ino == 1 {
            reply.error(EPERM);
            return;
        }

        // This is needed to populate the cache
        // TODO: improve
        let (_folders, _files) = self.folders_files();

        let orig_parent_dir = if let Some(x) = self
            .fs_mem
            .borrow()
            .nodes
            .get(&orig_parent_dir_ino)
            .unwrap()
            .upgrade()
        {
            x
        } else {
            reply.error(EIO);
            return;
        };

        let dest_parent_dir = if let Some(x) = self
            .fs_mem
            .borrow()
            .nodes
            .get(&dest_parent_dir_ino)
            .unwrap()
            .upgrade()
        {
            x
        } else {
            reply.error(EIO);
            return;
        };

        let (_orig_parent_dir_name, orig_name_prefix) = match &orig_parent_dir.borrow().file_type {
            GrizolFSFileType::File(f) => (
                f.file_info.name.clone(),
                format!("{}/{}", f.file_info.name, orig_name.to_str().unwrap()),
            ),
            GrizolFSFileType::Folder(f) => {
                (f.folder.id.clone(), orig_name.to_str().unwrap().to_string())
            }
        };

        let binding = orig_parent_dir.borrow();
        let orig_file = if let Some(x) =
            binding
                .children
                .iter()
                .find(|x| match &x.1.deref().borrow().file_type {
                    GrizolFSFileType::File(f) => {
                        // TODO: Use os stuff
                        // TODO: test also folder and other edge cases
                        f.file_info.name == orig_name_prefix
                    }
                    GrizolFSFileType::Folder(_f) => todo!(),
                }) {
            x
        } else {
            reply.error(ENOENT);
            return;
        };

        let mut collector: Collector = Default::default();
        let traverse_res = GrizolFsMem::traverse_post(orig_file.1, &mut collector);

        if let Err(e) = traverse_res {
            error!("Error occurred while traversing the dir tree: {}", e);
            reply.error(EIO);
            return;
        }

        let (_dest_parent_dir_name, dest_name_prefix) = match &dest_parent_dir.borrow().file_type {
            GrizolFSFileType::File(f) => (
                f.file_info.name.clone(),
                format!("{}/{}", f.file_info.name, dest_name.to_str().unwrap()),
            ),
            GrizolFSFileType::Folder(f) => {
                (f.folder.id.clone(), dest_name.to_str().unwrap().to_string())
            }
        };

        debug!("dest_name_prefix: {}", &dest_name_prefix);

        for orig_file in collector.vec.iter() {
            match &orig_file.upgrade().unwrap().deref().borrow().file_type {
                GrizolFSFileType::File(file) => {
                    // TODO: deal with paths not with strings
                    let orig_file_name = format!("{}", file.file_info.name);

                    assert!(
                        orig_file_name.starts_with(&orig_name_prefix),
                        "'{}' does not start with '{}'",
                        &orig_file_name,
                        &orig_name_prefix
                    );

                    let dest_file_name =
                        orig_file_name.replacen(&orig_name_prefix, &dest_name_prefix, 1);

                    debug!("orig_file_name: {}", &orig_file_name);
                    debug!("dest_file_name: {}", &dest_file_name);

                    self.rt.block_on(async {
                        // TODO: better handle errors
                        let file_locations = match self
                            .storage_manager
                            .cp_remote_to_remote(&file, &dest_file_name)
                            .await
                        {
                            Ok(x) => x,
                            Err(e) => {
                                error!("Occurred error when copying to remote: {}", &e);
                                return;
                            }
                        };

                        let mut state = self.state.lock().await;
                        state
                            .mv_file_info(
                                &file.folder,
                                &self.config.local_device_id,
                                &file.file_info,
                                &dest_file_name,
                                &file_locations,
                            )
                            .await;

                        match self
                            .storage_manager
                            .rm_remote(&file.folder, &file.file_info.name)
                            .await
                        {
                            Err(e) => {
                                error!("Occurred error when removing from remote: {}", &e);
                                return;
                            }
                            _ => {}
                        };
                    });
                }
                _ => {
                    assert!(
                        false,
                        "breaking an invariant, the file must be of type file"
                    );
                    return;
                }
            };
        }

        reply.ok();
    }

    fn rename_db(
        &mut self,
        _req: &Request<'_>,
        orig_parent_dir_ino: u64,
        orig_name: &OsStr,
        dest_parent_dir_ino: u64,
        dest_name: &OsStr,
        _flags: u32,
        reply: ReplyEmpty,
    ) {
        debug!(
            "Start moving {} {:?} to {} {:?}",
            &orig_parent_dir_ino, orig_name, &dest_parent_dir_ino, dest_name
        );

        let (folders, files) = self.folders_files();

        struct Dirs {
            top_folder: Option<String>,
            dir: Option<String>,
        }

        let orig_parent_dir: Option<Dirs> = if orig_parent_dir_ino == FUSE_ROOT_ID {
            Some(Dirs {
                top_folder: None,
                dir: None,
            })
        } else if orig_parent_dir_ino < ENTRIES_START {
            folders
                .iter()
                .find(|f| top_dir_id(f.id) == orig_parent_dir_ino)
                .map(|f| Dirs {
                    top_folder: Some(f.folder.id.clone()),
                    dir: None,
                })
        } else {
            files
                .iter()
                .find(|f| entry_id(f.id) == orig_parent_dir_ino)
                .map(|f| Dirs {
                    top_folder: Some(f.folder.clone()),
                    dir: Some(f.file_info.name.clone()),
                })
        };

        let dest_parent_dir: Option<Dirs> = if dest_parent_dir_ino == FUSE_ROOT_ID {
            Some(Dirs {
                top_folder: None,
                dir: None,
            })
        } else if dest_parent_dir_ino < ENTRIES_START {
            folders
                .iter()
                .find(|f| top_dir_id(f.id) == dest_parent_dir_ino)
                .map(|f| Dirs {
                    top_folder: Some(f.folder.id.clone()),
                    dir: None,
                })
        } else {
            files
                .iter()
                .find(|f| entry_id(f.id) == dest_parent_dir_ino)
                .map(|f| Dirs {
                    top_folder: Some(f.folder.clone()),
                    dir: Some(f.file_info.name.clone()),
                })
        };

        if orig_parent_dir.is_none() {
            debug!("Orig dir not found");
            return reply.error(ENOENT);
        }

        if dest_parent_dir.is_none() {
            debug!("Dest dir not found");
            return reply.error(ENOENT);
        }

        let orig_parent_dir = orig_parent_dir.unwrap();
        let dest_parent_dir = dest_parent_dir.unwrap();

        // The file that needs to be moved.
        let orig_grizol_file = files
            .iter()
            .filter(|f| {
                let is_top_dir_same = &orig_parent_dir
                    .top_folder
                    .as_ref()
                    .map(|d| d == &f.folder)
                    .unwrap_or(false);

                let is_dir_same = &orig_parent_dir
                    .dir
                    .as_ref()
                    .map(|d| format!("{}/{}", d, orig_name.to_str().unwrap()) == f.file_info.name)
                    .unwrap_or(orig_name.to_str().unwrap() == f.file_info.name);

                debug!(
                    "is_top_dir_same {}, {:?}/{} =?= {}",
                    is_top_dir_same,
                    &orig_parent_dir.dir,
                    orig_name.to_str().unwrap(),
                    f.file_info.name
                );
                debug!("Are dirs same: {}", *is_top_dir_same && *is_dir_same);

                *is_top_dir_same && *is_dir_same
            })
            .find(|&f| {
                is_file_in_current_dir(
                    orig_parent_dir
                        .dir
                        .as_ref()
                        .map(|x| x.to_string())
                        .unwrap_or("".to_string())
                        .as_str(),
                    f,
                )
            });

        if orig_grizol_file.is_none() {
            debug!("Origin file info not found");
            return reply.error(ENOENT);
        }
        let orig_grizol_file = orig_grizol_file.unwrap().clone();
        let orig_folder = &orig_grizol_file.folder;

        let dest_folder = folders.iter().find(|&f| {
            dest_parent_dir
                .top_folder
                .as_ref()
                .map(|tf| tf == &f.folder.id)
                .unwrap_or(false)
        });
        let dest_folder = if let Some(f) = dest_folder {
            f.folder.id.as_ref()
        } else {
            debug!("Origin file info not found");
            return reply.error(ENOENT);
        };

        self.rt.block_on(async {
            let mut state = self.state.lock().await;

            // TODO: this is terribly inefficient
            // FileInfo that will be copied in the index.
            let mut file = state
                .file_info(
                    orig_folder,
                    self.config.local_device_id,
                    &orig_grizol_file.file_info.name,
                )
                .await
                .unwrap();

            let orig_file_path = file.name;
            debug!(
                "orig_folder: {} - orig_file_path: {}",
                orig_folder, orig_file_path
            );

            // TODO: merge the data with Path and not manually
            // Path within a folder
            let dest_file_path = format!(
                "{}{}",
                dest_parent_dir
                    .dir
                    .clone()
                    .map(|x| format!("{}/", x))
                    .unwrap_or("".to_string()),
                dest_name.to_str().unwrap()
            );
            file.name = dest_file_path.clone();
            debug!(
                "dest_folder: {} - dest_file_path: {}",
                dest_folder, dest_file_path
            );

            let res = self
                .storage_manager
                // TODO: consider if we should use mv (and implement it) instead of cp so that we
                // leave the cp and rm mechanism to the backend? check the tradeoffs
                .cp(orig_folder, &orig_file_path, dest_folder, &dest_file_path)
                .await;

            if res.is_err() {
                error!("cannot move the files: {:?}", res);
                // TODO: potentially undo stuff
                // TODO: return more appropriate error e.g. reply.error(ENOENT);
            }

            state
                .insert_file_info(
                    dest_folder,
                    &self.config.local_device_id,
                    vec![file.clone()],
                )
                .await;

            state
                .update_file_locations(
                    dest_folder,
                    &self.config.local_device_id,
                    &file.name,
                    res.unwrap(),
                )
                .await;

            state
                .rm_file_info(orig_folder, &self.config.local_device_id, &orig_file_path)
                .await;

            self.storage_manager
                .rm_remote(orig_folder, &orig_file_path)
                .await
                .expect("Failed to remove file");
        });

        reply.ok();
    }
}

impl<TS: TimeSource<Utc>> Filesystem for GrizolFS<TS> {
    fn getattr(&mut self, req: &Request, ino: u64, _fh: Option<u64>, reply: ReplyAttr) {
        if self.config.fuse_inmem {
            self.getattr_mem(req, ino, reply);
        } else {
            self.getattr_db(req, ino, reply);
        }
    }

    fn lookup(
        &mut self,
        _req: &Request<'_>,
        parent_ino: u64,
        name: &std::ffi::OsStr,
        reply: fuser::ReplyEntry,
    ) {
        if self.config.fuse_inmem {
            self.lookup_mem(_req, parent_ino, name, reply)
        } else {
            self.lookup_db(_req, parent_ino, name, reply);
        }
    }

    fn readdir(
        &mut self,
        req: &Request<'_>,
        ino: u64,
        _fh: u64,
        offset: i64,
        reply: fuser::ReplyDirectory,
    ) {
        if self.config.fuse_inmem {
            self.readdir_mem(req, ino, _fh, offset, reply)
        } else {
            self.readdir_db(req, ino, _fh, offset, reply)
        }
    }

    fn rename(
        &mut self,
        _req: &Request<'_>,
        orig_parent_dir_ino: u64,
        orig_name: &OsStr,
        dest_parent_dir_ino: u64,
        dest_name: &OsStr,
        _flags: u32,
        reply: ReplyEmpty,
    ) {
        if self.config.fuse_inmem {
            self.rename_mem(
                _req,
                orig_parent_dir_ino,
                orig_name,
                dest_parent_dir_ino,
                dest_name,
                _flags,
                reply,
            )
        } else {
            self.rename_db(
                _req,
                orig_parent_dir_ino,
                orig_name,
                dest_parent_dir_ino,
                dest_name,
                _flags,
                reply,
            )
        }
    }

    fn read(
        &mut self,
        req: &Request<'_>,
        ino: u64,
        fh: u64,
        offset: i64,
        size: u32,
        flags: i32,
        lock_owner: Option<u64>,
        reply: ReplyData,
    ) {
        if self.config.fuse_inmem {
            self.read_mem(req, ino, fh, offset, size, flags, lock_owner, reply)
        } else {
            todo!();
        }
    }
}

pub fn mount<TS: TimeSource<Utc> + 'static>(
    mountpoint: &Path,
    fs: GrizolFS<TS>,
    // ) -> BackgroundSession {
) {
    let mut session = fuser::Session::new(
        fs,
        mountpoint,
        &[MountOption::AllowRoot, MountOption::AutoUnmount],
    )
    .unwrap();
    session.run().unwrap();
}

fn top_folder(ino: u64, folders: &[GrizolFolder], files: &[GrizolFileInfo]) -> Option<String> {
    if ino < ENTRIES_START {
        folders
            .iter()
            .find(|f| top_dir_id(f.id) == ino)
            .map(|f| f.folder.id.to_owned())
    } else {
        files
            .iter()
            .find(|&f| entry_id(f.id) == ino)
            .map(|f| f.folder.to_owned())
    }
}

fn parent_dir(files: &[GrizolFileInfo], parent_ino: u64) -> String {
    if parent_ino < ENTRIES_START {
        return "".to_owned();
    }
    files
        .iter()
        .find(|&x| entry_id(x.id) == parent_ino)
        .map(|x| &x.file_info.name)
        .unwrap()
        .to_owned()
}

fn base_dir(ino: u64, files: &[GrizolFileInfo]) -> Option<String> {
    if ino < ENTRIES_START {
        Some("".to_string())
    } else {
        files
            .iter()
            .find(|&f| entry_id(f.id) == ino)
            .map(|f| f.file_info.name.to_owned())
    }
}

fn is_file_in_current_dir(base_dir: &str, file: &GrizolFileInfo) -> bool {
    let stripped_path = Path::new(&file.file_info.name).strip_prefix(base_dir);
    if stripped_path.is_err() {
        return false;
    }
    let stripped_path = stripped_path.unwrap();

    let file_name = stripped_path.file_name();
    if file_name.is_none() {
        return false;
    }
    let file_name = file_name.unwrap();

    stripped_path == Path::new(file_name)
}

fn file_type(file_type_code: i32) -> FileType {
    match file_type_code {
        0 => FileType::RegularFile,
        1 => FileType::Directory,
        2..=4 => FileType::Symlink,
        _ => todo!(),
    }
}
