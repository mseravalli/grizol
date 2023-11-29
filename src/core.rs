// Include the `syncthing` module, which is generated from syncthing.proto.
// It is important to maintain the same structure as in the proto.

pub mod bep_data_parser;
pub mod bep_processor;
pub mod bep_state;

use crate::connectivity::OpenConnection;
use crate::core::bep_data_parser::{BepDataParser, CompleteMessage, MAGIC_NUMBER};
use crate::core::bep_state::BepState;
use crate::device_id::DeviceId;
use crate::storage;
use crate::storage::StorageManager;
use crate::syncthing;
use core::future::IntoFuture;
use futures::future::{Future, FutureExt};
use prost::Message;
use rand::prelude::*;
use sha2::{Digest, Sha256};
use sqlx::sqlite::{SqlitePool, SqliteQueryResult};
use std::array::TryFromSliceError;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::Write;
use std::path::Path;
use std::pin::Pin;
use std::time::{Duration, Instant};
use syncthing::{
    BlockInfo, Close, ClusterConfig, Counter, ErrorCode, FileInfo, Header, Hello, Index,
    IndexUpdate, MessageType, Ping, Request, Response,
};
use tokio::sync::Mutex;

const PING_INTERVAL: Duration = Duration::from_secs(45);

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

#[derive(Debug, Clone, PartialEq)]
enum UploadStatus {
    BlocksMissing,
    AllBlocks,
}

// TODO: initialize through a from config or something, it's probably easier
/// Data that will not change troughout the life ot the program.
#[derive(Debug, Clone)]
pub struct BepConfig {
    pub id: DeviceId,
    pub name: String,
    pub trusted_peers: HashSet<DeviceId>,
    pub base_dir: String,
    pub net_address: String,
}

type BepReply<'a> = Pin<Box<dyn Future<Output = EncodedMessages> + Send + 'a>>;
