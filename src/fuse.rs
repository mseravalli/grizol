use crate::core::bep_state::BepState;
use crate::core::{GrizolConfig, GrizolFileInfo, GrizolFolder};
use crate::storage::StorageManager;
use chrono::prelude::*;
use chrono_timesource::TimeSource;
use fuser::{
    BackgroundSession, FileAttr, FileType, Filesystem, MountOption, ReplyAttr, ReplyData,
    ReplyEmpty, Request, FUSE_ROOT_ID,
};
use libc::{EFBIG, EISDIR, ENOBUFS, ENOENT, ENOSYS};
use std::cell::Ref;
use std::cell::RefCell;
use std::ffi::OsStr;
use std::ops::Deref;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant, UNIX_EPOCH};
use tokio::runtime::Runtime;
use tokio::sync::Mutex;

// We allow 2^32 - 10 operations to be performed on the top dirs.
const ENTRIES_START: u64 = 1 << 32;
fn entry_id(e_id: i64) -> u64 {
    e_id as u64 + ENTRIES_START
}

const TOP_DIRS_START: u64 = 10;
fn top_dir_id(d_id: i64) -> u64 {
    let res = d_id as u64 + TOP_DIRS_START;
    if res >= ENTRIES_START {
        panic!("Too many operations performed on the top level dirs");
    }
    res
}

pub struct GrizolFS<TS: TimeSource<Utc>> {
    state: Arc<Mutex<BepState<TS>>>,
    config: GrizolConfig,
    // TODO: see if one of the other solutions is better in this case: https://tokio.rs/tokio/topics/bridging
    rt: Runtime,
    last_folders_files: RefCell<Instant>,
    folders: RefCell<Vec<GrizolFolder>>,
    files: RefCell<Vec<GrizolFileInfo>>,
    storage_manager: StorageManager,
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
        }
    }

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
}

impl<TS: TimeSource<Utc>> Filesystem for GrizolFS<TS> {
    fn getattr(&mut self, _req: &Request, ino: u64, reply: ReplyAttr) {
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

        if let Some(a) = attr {
            reply.attr(&self.config.fuse_refresh_rate, &a);
        } else {
            reply.error(ENOENT);
        }
    }

    fn lookup(
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

    fn readdir(
        &mut self,
        _req: &Request<'_>,
        ino: u64,
        _fh: u64,
        offset: i64,
        mut reply: fuser::ReplyDirectory,
    ) {
        debug!("Start reading dir with ino {}", ino);
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
                    debug!("The buffer is full");
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
                .rm(orig_folder, &orig_file_path)
                .await
                .expect("Failed to remove file");
        });

        reply.ok();
    }

    fn read(
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
        let (_folders, files) = self.folders_files();

        if ino < ENTRIES_START {
            reply.error(EISDIR);
            return;
        };

        // TODO: extend to symlinks
        let g_file = files
            .iter()
            .find(|&f| entry_id(f.id) == ino && f.file_info.r#type == 0);

        let g_file = if let Some(f) = g_file {
            f
        } else {
            // TODO: better error handling, distingush e.g. folders
            reply.error(EISDIR);
            return;
        };

        if u64::try_from(g_file.file_info.size).unwrap() > self.config.read_cache_size {
            reply.error(EFBIG);
            return;
        }

        debug!("file to be read: {:?}", g_file);

        let (folder, file_name) = (g_file.folder.clone(), g_file.file_info.name.clone());

        let is_dir = false;
        if is_dir {
            reply.error(ENOSYS);
            return;
        }

        let data = self.rt.block_on(async {
            let state = self.state.lock().await;
            if !state
                .is_file_in_read_cache(&self.config.local_device_id, &folder, &file_name)
                .await
            {
                let removed_files = state
                    .free_read_cache(
                        self.config.read_cache_size,
                        g_file.file_info.size.try_into().unwrap(),
                    )
                    .await
                    .expect("Failed to remove from database");
                debug!("removed_files: {:?}", removed_files);
                self.storage_manager
                    .free_read_cache(removed_files)
                    .await
                    .expect("Failed to remove files");
                self.storage_manager
                    .cp_remote_to_read_cache(&folder, &file_name)
                    .await
                    .expect("Failed to copy file");
            }
            state
                .update_read_cache(&self.config.local_device_id, &folder, &file_name)
                .await;
            self.storage_manager
                .read_cached(&folder, &file_name, offset, size)
                .await
                .expect("Failed to read data")
        });

        reply.data(&data);
    }
}

pub fn mount<TS: TimeSource<Utc> + 'static>(
    mountpoint: &Path,
    fs: GrizolFS<TS>,
) -> BackgroundSession {
    let session = fuser::Session::new(
        fs,
        mountpoint,
        &[MountOption::AllowRoot, MountOption::AutoUnmount],
    )
    .unwrap();
    session.spawn().unwrap()
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
