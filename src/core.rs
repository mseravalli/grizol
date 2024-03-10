// Include the `syncthing` module, which is generated from syncthing.proto.
// It is important to maintain the same structure as in the proto.

pub mod bep_data_parser;
pub mod bep_processor;
pub mod bep_state;

use crate::device_id::DeviceId;
use crate::grizol;
use crate::grizol::StorageStrategy;

use libc;
use std::collections::HashSet;
use std::convert::From;
use std::path::{Path, PathBuf};

// TODO: rethink this structure, e.g. if we should store CompleteMessages and encode them at the
// time of reading.
#[derive(Debug, Clone)]
pub struct EncodedMessages {
    data: Vec<u8>,
}

impl EncodedMessages {
    fn new(data: Vec<u8>) -> Self {
        EncodedMessages { data }
    }

    fn empty() -> Self {
        EncodedMessages { data: vec![] }
    }

    fn append(&mut self, em: Self) {
        self.data.extend_from_slice(em.data())
    }

    pub fn data(&self) -> &[u8] {
        &self.data[..]
    }
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
    pub mountpoint: Option<PathBuf>,
    pub uid: u32,
    pub gid: u32,
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

        let base_dir = if grizol_proto_config.base_dir.is_empty() {
            String::from("~/grizol")
        } else {
            grizol_proto_config.base_dir
        };

        let db_url = if grizol_proto_config.db_url.is_empty() {
            String::from("sqlite:~/.grizol.db")
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
            String::from("~/grizol")
        } else {
            grizol_proto_config.remote_base_dir
        };

        let mountpoint = if grizol_proto_config.mountpoint.is_empty() {
            None
        } else {
            Some(PathBuf::from(grizol_proto_config.mountpoint))
        };

        let uid = unsafe { libc::geteuid() };
        let gid = unsafe { libc::getegid() };

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
            mountpoint,
            uid,
            gid,
        }
    }
}
