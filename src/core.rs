// Include the `syncthing` module, which is generated from syncthing.proto.
// It is important to maintain the same structure as in the proto.

pub mod bep_data_parser;
pub mod bep_processor;
pub mod bep_state;
pub mod indices;
pub mod outgoing_requests;

use crate::core::bep_data_parser::CompleteMessage;
use crate::device_id::DeviceId;
use crate::grizol;
use crate::grizol::StorageStrategy;
use crate::syncthing::{FileInfo, Folder};
use parse_size::parse_size;
use std::collections::HashSet;
use std::convert::From;
use std::path::{Path, PathBuf};
use std::time::Duration;

#[derive(Debug, Clone)]
pub enum GrizolEvent {
    Message(CompleteMessage),
    RequestProcessed,
    RequestCreated,
}

// This struct allows us to include additional implementation specific information.
#[derive(Debug, Clone)]
pub struct GrizolFolder {
    pub folder: Folder,
    pub id: i64,
}

#[derive(Clone, Debug)]
pub enum StorageBackend {
    // TODO: include local
    // Local means that the file is stored on the local device.
    // Local,
    Remote(String),
}

#[derive(Clone, Debug)]
pub struct FileLocation {
    pub location: String,
    pub storage_backend: StorageBackend,
}

// This struct allows us to include additional implementation specific information.
#[derive(Debug, Clone, Default)]
pub struct GrizolFileInfo {
    pub file_info: FileInfo,
    pub id: i64,
    pub file_locations: Vec<FileLocation>,
    pub folder: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UploadStatus {
    BlocksMissing,
    AllBlocks,
}

// TODO: consider if we need to add getters for this to ensure by desing that the data once written
// cannot be modified. Maybe thorough https://crates.io/crates/getset or https://crates.io/crates/derive-getters
/// Data that will not change troughout the life ot the program.
#[derive(Debug, Clone)]
pub struct GrizolConfig {
    pub local_device_id: DeviceId,
    pub name: String,
    pub trusted_peers: HashSet<DeviceId>,
    pub local_base_dir: String,
    pub net_address: String,
    pub db_url: String,
    pub storage_strategy: StorageStrategy,
    pub rclone_config: Option<String>,
    pub remote_base_dir: String,
    pub read_cache_dir: String,
    pub read_cache_size: u64,
    pub mountpoint: Option<PathBuf>,
    pub fuse_refresh_rate: Duration,
    pub uid: u32,
    pub gid: u32,
    pub max_pending_requests: usize,
    pub max_total_pending_requests_size: usize,
    pub fuse_inmem: bool,
}

impl From<grizol::Config> for GrizolConfig {
    fn from(grizol_proto_config: grizol::Config) -> Self {
        let name = if grizol_proto_config.name.is_empty() {
            String::from("Grizol Server")
        } else {
            grizol_proto_config.name
        };

        let net_address = if grizol_proto_config.address.is_empty() {
            String::from("0.0.0.0:23456")
        } else {
            grizol_proto_config.address
        };

        // FIXME: this needs to be renamed
        let base_dir = if grizol_proto_config.base_dir.is_empty() {
            String::from("~/grizol/staging_area")
        } else {
            grizol_proto_config.base_dir
        };

        let db_url = if grizol_proto_config.db_url.is_empty() {
            String::from("sqlite:~/.grizol/grizol.db")
        } else {
            grizol_proto_config.db_url
        };

        let storage_strategy =
            grizol::StorageStrategy::try_from(grizol_proto_config.storage_strategy)
                .unwrap_or(grizol::StorageStrategy::Remote);

        let rclone_config = if grizol_proto_config.rclone_config.is_empty() {
            None
        } else {
            Some(grizol_proto_config.rclone_config)
        };

        let remote_base_dir = if grizol_proto_config.remote_base_dir.is_empty() {
            String::from("grizol")
        } else {
            grizol_proto_config.remote_base_dir
        };

        let read_cache_dir = if grizol_proto_config.read_cache_dir.is_empty() {
            String::from("~/.grizol/read_cache")
        } else {
            grizol_proto_config.read_cache_dir
        };

        let read_cache_size = if grizol_proto_config.read_cache_size.is_empty() {
            1_000_000_000 // 1GB
        } else {
            match parse_size(grizol_proto_config.read_cache_size.clone()) {
                Ok(size) => size,
                Err(e) => panic!(
                    "Cannot convert '{}' to bytes: {}",
                    grizol_proto_config.read_cache_size, e
                ),
            }
        };

        let mountpoint = if grizol_proto_config.mountpoint.is_empty() {
            None
        } else {
            Some(PathBuf::from(grizol_proto_config.mountpoint))
        };

        let fuse_refresh_rate = if let Some(d) = grizol_proto_config.fuse_refresh_rate_ms {
            Duration::from_millis(d.into())
        } else {
            Duration::from_millis(0)
        };

        let uid = unsafe { libc::geteuid() };
        let gid = unsafe { libc::getegid() };

        let max_pending_requests = if let Some(x) = grizol_proto_config.max_pending_requests {
            x.try_into().unwrap()
        } else {
            10_000
        };

        let max_total_pending_requests_size = if grizol_proto_config
            .max_total_pending_requests_size
            .is_empty()
        {
            1_000_000_000 // 1GB
        } else {
            match parse_size(grizol_proto_config.max_total_pending_requests_size.clone()) {
                Ok(size) => size.try_into().unwrap(),
                Err(e) => panic!(
                    "Cannot convert '{}' to bytes: {}",
                    grizol_proto_config.max_total_pending_requests_size, e
                ),
            }
        };

        GrizolConfig {
            local_device_id: DeviceId::from(Path::new(grizol_proto_config.cert.as_str())),
            name,
            trusted_peers: grizol_proto_config
                .trusted_peers
                .iter()
                .map(|x| DeviceId::try_from(x.as_str()).unwrap())
                .collect(),
            local_base_dir: base_dir,
            net_address,
            db_url,
            storage_strategy,
            rclone_config,
            remote_base_dir,
            read_cache_dir,
            read_cache_size,
            mountpoint,
            fuse_refresh_rate,
            uid,
            gid,
            max_pending_requests,
            max_total_pending_requests_size,
            fuse_inmem: grizol_proto_config.fuse_inmem,
        }
    }
}
