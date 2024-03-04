use crate::core::bep_state::{BepState, FileLocation};
use crate::core::GrizolConfig;
use crate::syncthing::FileInfo;
use chrono::prelude::*;
use chrono::prelude::*;
use chrono_timesource::TimeSource;
use fuser::{
    BackgroundSession, FileAttr, FileType, Filesystem, MountOption, ReplyAttr, ReplyOpen, Request,
    Session, FUSE_ROOT_ID,
};
use libc::{EACCES, EINVAL, EISDIR, ENOBUFS, ENOENT, ENOTDIR};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, UNIX_EPOCH};
use tokio::runtime::Runtime;
use tokio::sync::Mutex;

const HELLO_DIR_ATTR: FileAttr = FileAttr {
    ino: 1,
    size: 0,
    blocks: 0,
    atime: UNIX_EPOCH, // 1970-01-01 00:00:00
    mtime: UNIX_EPOCH,
    ctime: UNIX_EPOCH,
    crtime: UNIX_EPOCH,
    kind: FileType::Directory,
    perm: 0o755,
    nlink: 2,
    uid: 501,
    gid: 20,
    rdev: 0,
    flags: 0,
    blksize: 512,
};

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
}

impl<TS: TimeSource<Utc>> Filesystem for GrizolFS<TS> {
    fn getattr(&mut self, req: &Request, ino: u64, reply: ReplyAttr) {
        reply.attr(&Duration::from_secs(100), &HELLO_DIR_ATTR);
    }

    fn readdir(
        &mut self,
        _req: &Request<'_>,
        ino: u64,
        fh: u64,
        offset: i64,
        mut reply: fuser::ReplyDirectory,
    ) {
        let offset = offset as usize;

        // TODO: store the result, fh set with opendir should help with this
        let files = self.rt.block_on(async {
            let state = self.state.lock().await;

            // FIXME: pass dir, 1 fuse per folder??
            state
                .files_fuse("orig_dir", self.config.local_device_id)
                .await
        });

        let base_dir: String = if ino == FUSE_ROOT_ID {
            "".to_string()
        } else {
            todo!();
        };

        let files: Vec<(FileInfo, Vec<FileLocation>)> = files
            .into_iter()
            .filter(|(file, _)| is_file_in_current_dir(&base_dir, &file))
            .collect();

        debug!("offset: {:?}, files.len: {:?}", offset, files.len());

        if offset == files.len() {
            reply.ok();
            return;
        }

        for i in offset..files.len() {
            let file = &files[i].0;

            debug!("Processing: {:?}", file.name);
            let is_buffer_full = reply.add(
                file.sequence as u64,
                (i + 1) as i64,
                // TODO: use the correct file type
                FileType::RegularFile
                file.name.clone(),
                // format!("file-{}", file.sequence),
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
        &mountpoint,
        &[MountOption::AllowRoot, MountOption::AutoUnmount],
    )
    .unwrap();
    session.spawn().unwrap()
}

fn is_file_in_current_dir(base_dir: &str, file: &FileInfo) -> bool {
    let stripped_path = Path::new(&file.name).strip_prefix(&base_dir);
    if stripped_path.is_err() {
        return false;
    }
    let stripped_path = stripped_path.unwrap();

    let file_name = stripped_path.file_name();
    if file_name.is_none() {
        return false;
    }
    let file_name = file_name.unwrap();

    let res = stripped_path == Path::new(file_name);
    debug!(
        "stripped_path: {:?} - file_name: {:?} - res: {:?}",
        stripped_path, file_name, res
    );
    res
}
