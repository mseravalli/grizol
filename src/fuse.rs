use crate::core::bep_state::{BepState, FileLocation};
use crate::core::GrizolConfig;
use crate::syncthing::FileInfo;
use chrono::prelude::*;

use chrono_timesource::TimeSource;
use fuser::{
    BackgroundSession, FileAttr, FileType, Filesystem, MountOption, ReplyAttr, Request,
    FUSE_ROOT_ID,
};
use libc::{ENOBUFS, ENOENT};
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, UNIX_EPOCH};
use tokio::runtime::Runtime;
use tokio::sync::Mutex;

const TOP_DIRS_START: u64 = 10;
const ENTRIES_START: u64 = 1000;

pub struct GrizolFS<TS: TimeSource<Utc>> {
    state: Arc<Mutex<BepState<TS>>>,
    config: GrizolConfig,
    // TODO: see if one of the other solutions is better in this case: https://tokio.rs/tokio/topics/bridging
    rt: Runtime,
}

impl<TS: TimeSource<Utc>> GrizolFS<TS> {
    pub fn new(config: GrizolConfig, state: Arc<Mutex<BepState<TS>>>) -> Self {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();

        GrizolFS { config, state, rt }
    }

    fn root_dir_attr(&self) -> FileAttr {
        FileAttr {
            ino: FUSE_ROOT_ID,
            size: 0,
            blocks: 0,
            atime: UNIX_EPOCH, // 1970-01-01 00:00:00
            mtime: UNIX_EPOCH,
            ctime: UNIX_EPOCH,
            crtime: UNIX_EPOCH,
            kind: FileType::Directory,
            perm: 0o755,
            nlink: 2,
            // TODO: use queying user
            uid: self.config.uid,
            gid: self.config.gid,
            rdev: 0,
            flags: 0,
            blksize: 512,
        }
    }

    fn attr_from_file(&self, file: &FileInfo) -> FileAttr {
        let modified_ts = UNIX_EPOCH
            .checked_add(Duration::from_secs(file.modified_s as u64))
            .and_then(|ts| ts.checked_add(Duration::from_nanos(file.modified_ns as u64)))
            .unwrap();
        let res = FileAttr {
            ino: file.sequence as u64,
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
            reply.attr(&Duration::from_secs(3600), &self.root_dir_attr());
            return;
        }

        // TODO: cache the result, fh set with opendir should help with this
        let files = self.rt.block_on(async {
            let state = self.state.lock().await;

            // FIXME: pass dir, 1 fuse per folder??
            state
                .files_fuse("orig_dir", self.config.local_device_id)
                .await
        });

        let file = if let Some(f) = files.iter().find(|&f| f.0.sequence == ino as i64) {
            f
        } else {
            reply.error(ENOENT);
            return;
        };

        let attr = self.attr_from_file(&file.0);
        reply.attr(&Duration::from_secs(3600), &attr);
    }

    fn lookup(
        &mut self,
        _req: &Request<'_>,
        parent_ino: u64,
        name: &std::ffi::OsStr,
        reply: fuser::ReplyEntry,
    ) {
        // TODO: query this from the database
        let files = self.rt.block_on(async {
            let state = self.state.lock().await;
            state
                .files_fuse("orig_dir", self.config.local_device_id)
                .await
        });

        let parent_dir = parent_dir(&files, parent_ino);

        let file = files
            .iter()
            .filter(|f| f.0.name.contains(name.to_str().unwrap()))
            .find(|&f| is_file_in_current_dir(&parent_dir, &f.0));

        if file.is_none() {
            reply.error(ENOENT);
            return;
        }
        let file = file.unwrap();

        let attr = self.attr_from_file(&file.0);

        reply.entry(&Duration::from_secs(3600), &attr, 1);
    }

    fn readdir(
        &mut self,
        _req: &Request<'_>,
        ino: u64,
        _fh: u64,
        offset: i64,
        mut reply: fuser::ReplyDirectory,
    ) {
        // TODO: cache the result, fh set with opendir should help with this
        let files = self.rt.block_on(async {
            let state = self.state.lock().await;

            // FIXME: pass dir, 1 fuse per folder??
            state
                .files_fuse("orig_dir", self.config.local_device_id)
                .await
        });

        debug!(
            "files {:?}",
            files
                .iter()
                .map(|f| f.0.name.clone())
                .collect::<Vec<String>>()
        );

        let base_dir: String = if ino == FUSE_ROOT_ID {
            "".to_string()
        } else {
            let res = files
                .iter()
                .find(|&f| f.0.sequence == ino as i64)
                .map(|f| f.0.name.to_owned());
            if res.is_none() {
                reply.error(ENOENT);
                return;
            }
            res.unwrap()
        };

        let offset = offset as usize;

        // TODO: should probably use filter_map here
        let files: Vec<(FileInfo, Vec<FileLocation>)> = files
            .into_iter()
            .filter(|(file, _)| is_file_in_current_dir(&base_dir, file))
            .collect();

        if offset == files.len() {
            reply.ok();
            return;
        }

        for (i, record) in files.iter().enumerate().skip(offset) {
            let file = &record.0;

            let file_name = Path::new(&file.name)
                .strip_prefix(Path::new(&base_dir))
                .unwrap()
                .as_os_str();

            let is_buffer_full = reply.add(
                file.sequence as u64,
                (i + 1) as i64,
                file_type(file.r#type),
                // TODO: remove base_dir from the name
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

fn parent_dir(files: &[(FileInfo, Vec<FileLocation>)], parent_ino: u64) -> String {
    if parent_ino == FUSE_ROOT_ID {
        return "".to_owned();
    }
    files
        .iter()
        .find(|&x| x.0.sequence == parent_ino as i64)
        .map(|x| &x.0.name)
        .unwrap()
        .to_owned()
}

fn is_file_in_current_dir(base_dir: &str, file: &FileInfo) -> bool {
    let stripped_path = Path::new(&file.name).strip_prefix(base_dir);
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
