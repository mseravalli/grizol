use crate::core::bep_state::BepState;
use crate::core::GrizolConfig;
use chrono::prelude::*;
use chrono::prelude::*;
use chrono_timesource::TimeSource;
use fuser::{
    BackgroundSession, FileAttr, FileType, Filesystem, MountOption, ReplyAttr, ReplyOpen, Request,
    Session,
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
        // TODO: store the result, fh should help with this
        let files = self.rt.block_on(async {
            let state = self.state.lock().await;

            // FIXME: pass dir, 1 fuse per folder??
            state
                .files_fuse("orig_dir", self.config.local_device_id)
                .await
        });

        debug!("offset: {:?}, files.len: {:?}", offset, files.len());

        let offset = offset as usize;

        if offset == files.len() {
            reply.ok();
            return;
        }

        for i in offset..files.len() {
            let file = &files[i].0;

            if file.name.starts_with("dir") {
                continue;
            }

            debug!("Processing: {:?}", file.name);
            let is_buffer_full = reply.add(
                file.sequence as u64,
                (i + 1) as i64,
                FileType::RegularFile,
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

        // let last_item_idx = 0;
        // if offset == 0 {
        //     let is_buffer_full = reply.add(2, last_item_idx + 1, FileType::RegularFile, "file1");
        //     if is_buffer_full {
        //         debug!("replied with full buffer");
        //         reply.error(ENOBUFS);
        //     } else {
        //         reply.ok();
        //     }
        // } else {
        //     reply.ok();
        // }
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
