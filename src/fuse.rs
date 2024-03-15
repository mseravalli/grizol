use crate::core::bep_state::BepState;
use crate::core::{GrizolConfig, GrizolFileInfo, GrizolFolder};
use chrono::prelude::*;
use chrono_timesource::TimeSource;
use fuser::{
    BackgroundSession, FileAttr, FileType, Filesystem, MountOption, ReplyAttr, Request,
    FUSE_ROOT_ID,
};
use libc::{ENOBUFS, ENOENT};
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant, UNIX_EPOCH};
use tokio::runtime::Runtime;
use tokio::sync::Mutex;

const REFRESH_RATE: Duration = Duration::from_secs(2);

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
    last_folders_files: Instant,
    folders: Vec<GrizolFolder>,
    files: Vec<GrizolFileInfo>,
}

impl<TS: TimeSource<Utc>> GrizolFS<TS> {
    pub fn new(config: GrizolConfig, state: Arc<Mutex<BepState<TS>>>) -> Self {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();

        GrizolFS {
            config,
            state,
            rt,
            // We set the instant to a point in time in the past to guarantee that the first time
            // this will be called it will be refreshed. Maybe there is a better way.
            last_folders_files: Instant::now()
                .checked_sub(REFRESH_RATE.mul_f64(2.0))
                .unwrap(),
            folders: Default::default(),
            files: Default::default(),
        }
    }

    fn refresh_expired_folders_files(&mut self) {
        if self.last_folders_files.elapsed() >= REFRESH_RATE {
            let (folders, files) = self.rt.block_on(async {
                let state = self.state.lock().await;

                let folders = state.top_folders_fuse(self.config.local_device_id).await;
                let files = state.files_fuse(self.config.local_device_id).await;
                (folders, files)
            });

            self.folders = folders;
            self.files = files;
        }
    }

    // This MUST be used in combination with refresh_expired_folders_files, the methods at the
    // moment are separated because there are issues with the borrow checker and I haven't found a better way to deal with this for now (https://stackoverflow.com/questions/78157006/rust-return-immutable-borrow-after-modifying) .
    fn folders_files(&self) -> (&[GrizolFolder], &[GrizolFileInfo]) {
        (&self.folders, &self.files)
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
        debug!("attr: {:?}", res);
        res
    }
}

impl<TS: TimeSource<Utc>> Filesystem for GrizolFS<TS> {
    fn getattr(&mut self, _req: &Request, ino: u64, reply: ReplyAttr) {
        if ino == FUSE_ROOT_ID {
            reply.attr(&REFRESH_RATE, &self.root_dir_attr());
            return;
        }

        self.refresh_expired_folders_files();
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
            reply.attr(&REFRESH_RATE, &a);
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
        self.refresh_expired_folders_files();
        let (folders, files) = self.folders_files();

        let attr = if parent_ino == FUSE_ROOT_ID {
            folders
                .iter()
                .find(|f| f.folder.id.contains(name.to_str().unwrap()))
                .map(|f| self.attr_from_folder(f))
        } else {
            let parent_dir = parent_dir(files, parent_ino);

            files
                .iter()
                .filter(|f| f.file_info.name.contains(name.to_str().unwrap()))
                .find(|&f| is_file_in_current_dir(&parent_dir, f))
                .map(|f| self.attr_from_file(f))
        };

        if let Some(a) = attr {
            reply.entry(&REFRESH_RATE, &a, 1);
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
        let offset = offset as usize;

        if ino == FUSE_ROOT_ID {
            let folders = self.rt.block_on(async {
                let state = self.state.lock().await;

                state.top_folders_fuse(self.config.local_device_id).await
            });

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
                    reply.error(ENOBUFS);
                    return;
                }
            }

            reply.ok();
            return;
        }

        self.refresh_expired_folders_files();
        let (folders, files) = self.folders_files();

        let top_folder = top_folder(ino, folders, files);
        let base_dir = base_dir(ino, files);

        if top_folder.as_ref().and(base_dir.as_ref()).is_none() {
            reply.error(ENOENT);
            return;
        }

        let top_folder = top_folder.unwrap();
        let base_dir = base_dir.unwrap();

        // TODO: should probably use filter_map here
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
                reply.error(ENOBUFS);
                return;
            }
        }

        reply.ok();
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
