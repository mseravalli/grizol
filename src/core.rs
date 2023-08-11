// Include the `syncthing` module, which is generated from syncthing.proto.
// It is important to maintain the same structure as in the proto.

pub mod bep_data_parser;

use crate::connectivity::OpenConnection;
use crate::core::bep_data_parser::{BepDataParser, CompleteMessage, MAGIC_NUMBER};
use crate::device_id::DeviceId;
use crate::storage;
use crate::storage::StorageManager;
use crate::syncthing;
use core::future::IntoFuture;
use prost::Message;
use rand::prelude::*;
use std::array::TryFromSliceError;
use std::collections::{BTreeMap, HashMap, HashSet};
// use std::future::Future;
use futures::future::{Future, FutureExt};
use std::io::Write;
use std::path::Path;
use std::pin::Pin;
use std::time::{Duration, Instant};
use syncthing::{
    BlockInfo, Close, ClusterConfig, FileInfo, Header, Hello, Index, IndexUpdate, MessageType,
    Ping, Request, Response,
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

// TODO: initialize through a from config or something, it's probably easier
/// Data that will not change troughout the life ot the program.
pub struct BepConfig {
    pub id: DeviceId,
    pub name: String,
    pub trusted_peers: HashSet<DeviceId>,
    pub base_dir: String,
    pub net_address: String,
}

struct BepState {
    indices: HashMap<String, HashMap<DeviceId, syncthing::Index>>,
    cluster: ClusterConfig,
    sequence: u64,
}

impl BepState {
    fn update_cluster_config(&mut self, my_device_id: DeviceId, other: &ClusterConfig) {
        for other_folder in other.folders.iter() {
            if !self.cluster.folders.iter().any(|f| f.id == other_folder.id) {
                if other_folder.devices.iter().any(|d| {
                    DeviceId::try_from(&d.id[..]).expect("Cannot convert to DeviceId")
                        == my_device_id
                }) {
                    self.cluster.folders.push(other_folder.clone());

                    self.indices
                        .entry(other_folder.id.clone())
                        .or_insert(HashMap::new());

                    let mut folder_devices = self.indices.get_mut(&other_folder.id).expect(
                        &format!("Entry for folder {} must be there", &other_folder.id),
                    );
                    for device in other_folder.devices.iter() {
                        let device_id =
                            DeviceId::try_from(&device.id).expect("Wrong device id format");

                        folder_devices.entry(device_id).or_insert(Index::default());
                    }
                }
            }
        }
    }
}

type BepReply<'a> = Pin<Box<dyn Future<Output = EncodedMessages> + Send + 'a>>;

// TODO: maybe change name to something like BepConnectionHandler
pub struct BepProcessor {
    config: BepConfig,
    state: Mutex<BepState>,
    storage_manager: StorageManager,
}

impl BepProcessor {
    pub fn new(config: BepConfig) -> Self {
        let bep_state = BepState {
            indices: Default::default(),
            cluster: ClusterConfig { folders: vec![] },
            sequence: 0,
        };

        BepProcessor {
            config,
            state: Mutex::new(bep_state),
            storage_manager: StorageManager::new(
                format!("/tmp/grizol_cluster_config"),
                format!("/tmp/grizol_staging"),
            ),
        }
    }

    pub fn handle_complete_message(&self, complete_message: CompleteMessage) -> Vec<BepReply> {
        match complete_message {
            CompleteMessage::Hello(x) => self.handle_hello(x),
            CompleteMessage::ClusterConfig(x) => self.handle_cluster_config(x),
            CompleteMessage::Index(x) => self.handle_index(x),
            CompleteMessage::IndexUpdate(x) => self.handle_index_update(x),
            CompleteMessage::Request(x) => self.handle_request(x),
            CompleteMessage::Response(x) => self.handle_response(x),
            CompleteMessage::DownloadProgress(x) => todo!(),
            CompleteMessage::Ping(x) => self.handle_ping(x),
            CompleteMessage::Close(x) => self.handle_close(x),
        }
    }

    fn handle_hello(&self, hello: Hello) -> Vec<BepReply> {
        debug!("Handling Hello");
        vec![
            Box::pin(self.hello()),
            Box::pin(self.cluster_config()),
            Box::pin(self.index()),
        ]
    }

    fn handle_cluster_config(&self, cluster_config: ClusterConfig) -> Vec<BepReply> {
        debug!("Handling Cluster Config");
        trace!("Received Cluster Config: {:#?}", &cluster_config);

        let res = async move {
            self.state
                .lock()
                .await
                .update_cluster_config(self.config.id, &cluster_config);
            {
                let cc = &self.state.lock().await.cluster;
                // debug!("Self cluster config: {:#?}", cc);
                self.storage_manager.save_cluster_config(cc).await;
            }

            EncodedMessages::empty()
        };

        vec![Box::pin(res)]
    }

    fn handle_index_update(&self, index_update: IndexUpdate) -> Vec<BepReply> {
        debug!("Handling Index Update");
        let index = Index {
            folder: index_update.folder,
            files: index_update.files,
        };
        self.handle_index(index)
    }

    fn handle_index(&self, index: Index) -> Vec<BepReply> {
        debug!("Handling Index");
        // debug!("Received Index: {:#?}", &index);

        let ems = async move {
            let missing_files = {
                let indices = &self.state.lock().await.indices;
                let local_index = indices
                    .get(&index.folder)
                    .map(|x| x.get(&self.config.id))
                    .flatten()
                    .expect(&format!(
                        "The index for device {} should be present",
                        &self.config.id
                    ));

                diff_indices(&local_index, &index)
            };

            let requests = create_requests(&index.folder, missing_files);
            let mut ems = EncodedMessages::empty();

            let header = Header {
                compression: 0,
                r#type: MessageType::Request.into(),
            };
            for request in requests.into_iter() {
                ems.append(encode_message(header.clone(), request).unwrap());
            }
            ems
        };

        vec![Box::pin(ems)]

        // let header = Header {
        //     compression: 0,
        //     r#type: MessageType::Request.into(),
        // };

        // self.storage_manager.save_client_index(&index);

        // let mut res: Vec<BepReply> = vec![];

        // for file in index.files.iter() {
        //     for block in file.blocks.iter() {
        //         let id = rand::random::<i32>().abs();
        //         let request = Request {
        //             id,
        //             folder: index.folder.clone(),
        //             name: file.name.clone(),
        //             offset: block.offset,
        //             size: block.size,
        //             hash: block.hash.clone(),
        //             from_temporary: false,
        //         };
        //         debug!("Sending Request");
        //         debug!("Sending Request {:?}", request);
        //         let header_clone = header.clone();
        //         let em = async move { self.encode_message(header_clone, request).unwrap() };
        //         res.push(Box::pin(em));
        //     }
        // }
        // res
    }

    fn handle_request(&self, request: Request) -> Vec<BepReply> {
        debug!("Handling Request");
        debug!("{:?}", request);

        vec![]

        // let request_folder = if request.folder == self.index.folder {
        //     Ok(&request.folder)
        // } else {
        //     Err(ErrorCode::Generic)
        // };

        // let file = request_folder.and_then(|x| {
        //     self.index
        //         .files
        //         .iter()
        //         .find(|&x| x.name == request.name)
        //         .ok_or(ErrorCode::NoSuchFile)
        // });

        // let data = file.and_then(|f| {
        //     let block = f
        //         .blocks
        //         .iter()
        //         .find(|&b| {
        //             b.offset == request.offset && b.size == request.size && b.hash == request.hash
        //         })
        //         .ok_or(ErrorCode::NoSuchFile);

        //     block.and_then(|b| {
        //         storage::data_from_file_block(
        //             // FIXME: track the dir somewhere else
        //             "/home/marco/workspace/hic-sunt-leones/syncthing-test",
        //             &f,
        //             &b,
        //         )
        //         .map_err(|e| ErrorCode::InvalidFile)
        //     })
        // });

        // let code: i32 = data
        //     .as_ref()
        //     .err()
        //     .unwrap_or(&ErrorCode::NoError)
        //     .to_owned()
        //     .into();

        // let response = Response {
        //     id: request.id,
        //     data: data.unwrap_or(Vec::new()),
        //     code,
        // };
        // debug!("Sending Response");
        // // debug!("Sending Response {:?}", response);

        // let header = Header {
        //     compression: 0,
        //     r#type: MessageType::Response.into(),
        // };
        // vec![Box::pin(self.encode_message(header, response).unwrap())]
    }

    fn handle_response(&self, response: Response) -> Vec<BepReply> {
        debug!("Handling Response");
        debug!(
            "Received Response: {:?}, {}",
            response.id,
            response.data.len()
        );
        // TODO: store the outgoing requests and remove them once they are done, check if the
        // checksum corresponds.
        vec![]
    }

    fn handle_ping(&self, ping: Ping) -> Vec<BepReply> {
        debug!("Handling Ping");
        trace!("{:?}", ping);
        vec![]
    }

    fn handle_close(&self, close: Close) -> Vec<BepReply> {
        debug!("Handling Close");
        trace!("{:?}", close);
        vec![]
    }

    async fn hello(&self) -> EncodedMessages {
        let hello = Hello {
            device_name: self.config.name.clone(),
            client_name: env!("CARGO_PKG_NAME").to_string(),
            client_version: env!("CARGO_PKG_VERSION").to_string(),
        };

        let message_bytes = hello.encode_to_vec();
        let message_len: u16 = message_bytes.len().try_into().unwrap();

        trace!("{:#04x?}", &hello.encode_to_vec());

        let message: Vec<u8> = vec![]
            .into_iter()
            .chain(MAGIC_NUMBER.into_iter())
            .chain(message_len.to_be_bytes().into_iter())
            .chain(message_bytes.into_iter())
            .collect();

        EncodedMessages::new(message)
    }

    // TODO: read this from the last state and add additional information from the config.
    async fn cluster_config(&self) -> EncodedMessages {
        let header = Header {
            compression: 0,
            r#type: MessageType::ClusterConfig.into(),
        };

        // let max_sequence: i64 = self.state.lock().await.sequence.try_into().unwrap();

        // let this_device = Device {
        //     id: self.config.id.into(),
        //     name: self.config.name.clone(),
        //     addresses: vec![self.config.net_address.clone()],
        //     compression: Compression::Never.into(),
        //     max_sequence,
        //     // Delta Index Exchange is not supported yet hence index_id is zero.
        //     index_id: 0,
        //     cert_name: String::new(),
        //     encryption_password_token: vec![],
        //     introducer: false,
        //     skip_introduction_removals: true,
        //     // ..Default::default()
        // };

        // // FIXME: This is an ugly hack to get the client id fix the workflow.
        // let client_id = self.config.trusted_peers.iter().next().unwrap();
        // let client_device = Device {
        //     id: client_id.into(),
        //     name: format!("syncthing"),
        //     addresses: vec![format!("127.0.0.1:220000")],
        //     compression: Compression::Never.into(),
        //     max_sequence: 100,
        //     // Delta Index Exchange is not supported yet hence index_id is zero.
        //     index_id: 0,
        //     cert_name: String::new(),
        //     encryption_password_token: vec![],
        //     introducer: false,
        //     skip_introduction_removals: true,
        //     // ..Default::default()
        // };

        // let folder_name = "test_a";

        // let folder = Folder {
        //     id: folder_name.to_string(),
        //     label: folder_name.to_string(),
        //     read_only: false,
        //     ignore_permissions: false,
        //     ignore_delete: true,
        //     disable_temp_indexes: false,
        //     paused: false,
        //     devices: vec![this_device, client_device],
        //     // ..Default::default()
        // };

        // let cluster_config = ClusterConfig { folders: vec![] };
        let cluster_config = self
            .storage_manager
            .restore_cluster_config()
            .await
            .expect("something went wrong when restoring");

        trace!("Sending Cluster Config: {:?}", &cluster_config);
        encode_message(header, cluster_config).unwrap()
    }

    pub async fn index(&self) -> EncodedMessages {
        let header = Header {
            compression: 0,
            r#type: MessageType::Index.into(),
        };

        let index = Index {
            folder: format!("vqick-icdkt"),
            files: vec![],
        };

        debug!("Sending Index");
        encode_message(header, index).unwrap()
    }

    pub async fn ping(&self) -> EncodedMessages {
        let header = Header {
            compression: 0,
            r#type: MessageType::Ping.into(),
        };

        let ping = Ping {};

        debug!("Sending Ping");
        encode_message(header, ping).unwrap()
    }

    fn update_index(&mut self, index: Index) -> Result<Vec<Request>, String> {
        todo!();
    }
}

fn encode_message<T: prost::Message>(
    header: Header,
    message: T,
) -> Result<EncodedMessages, String> {
    let header_bytes: Vec<u8> = header.encode_to_vec();
    let header_len: u16 = header_bytes.len().try_into().unwrap();

    let message_bytes = message.encode_to_vec();
    let message_len: u32 = message_bytes.len().try_into().unwrap();

    trace!(
        "Sending message with header len: {:?}, {:02x?}",
        header_len,
        header_len.to_be_bytes().into_iter().collect::<Vec<u8>>()
    );

    let message: Vec<u8> = vec![]
        .into_iter()
        .chain(header_len.to_be_bytes().into_iter())
        .chain(header_bytes.into_iter())
        .chain(message_len.to_be_bytes().into_iter())
        .chain(message_bytes.into_iter())
        .collect();

    trace!(
        "Sending message with len: {:?}, {:02x?}",
        message_len,
        message_len.to_be_bytes().into_iter().collect::<Vec<u8>>()
    );
    trace!(
        // "Outgoing message: {:#04x?}",
        // "Outgoing message: {:02x?}",
        "Outgoing message: {:?}",
        &message.clone().into_iter().collect::<Vec<u8>>()
    );

    Ok(EncodedMessages { data: message })
}

/// Returns a set of files present in [patch] that are not present in [base]. The returned files
/// cointain only the blocks not present in [base].
fn diff_indices(base: &Index, patch: &Index) -> Vec<FileInfo> {
    // TODO: store the conflicting files in the state
    let mut conflicting_files: Vec<FileInfo> = vec![];
    let mut missing_files: Vec<FileInfo> = vec![];
    let base_file_map: HashMap<&String, &FileInfo> =
        base.files.iter().map(|x| (&x.name, x)).collect();

    // TODO: remove this, we can just use the iterator in the for loop
    let patch_file_map: HashMap<&String, &FileInfo> =
        patch.files.iter().map(|x| (&x.name, x)).collect();

    for (name, patch_file_info) in patch_file_map.into_iter() {
        if let Some(base_file_info) = base_file_map.get(name) {
            let max_patch_version = patch_file_info
                .version
                .as_ref()
                .expect("The file must be versioned")
                .counters
                .iter()
                .max_by_key(|c| c.value)
                .expect("The file must be versioned");
            let max_base_version = base_file_info
                .version
                .as_ref()
                .expect("The file must be versioned")
                .counters
                .iter()
                .max_by_key(|c| c.value)
                .expect("The file must be versioned");
            if max_patch_version.value == max_base_version.value
                && max_patch_version.id != max_base_version.id
            {
                conflicting_files.push(patch_file_info.clone());
            } else if max_patch_version.value == max_base_version.value
                && max_patch_version.id == max_base_version.id
            {
                // Not all blocks have been copied yet
                if base_file_info.blocks.len()
                    < ((patch_file_info.size + patch_file_info.block_size as i64 - 1)
                        / (patch_file_info.block_size as i64)) as usize
                {
                    let base_file_blocks: HashMap<&Vec<u8>, &BlockInfo> =
                        base_file_info.blocks.iter().map(|b| (&b.hash, b)).collect();

                    let mut missing_blocks: Vec<BlockInfo> = vec![];

                    for patch_block in patch_file_info.blocks.iter() {
                        if base_file_blocks.get(&patch_block.hash).is_none() {
                            missing_blocks.push(patch_block.clone());
                        }
                    }

                    // We must have found some differences
                    assert!(missing_blocks.len() > 0);

                    let mut missing_file = patch_file_info.clone();
                    missing_file.blocks = missing_blocks;

                    missing_files.push(missing_file);
                }
            } else if max_patch_version.value > max_base_version.value {
                missing_files.push(patch_file_info.clone());
            }
        } else {
            missing_files.push(patch_file_info.clone());
        }
    }

    debug!("Missing Files: {:?}", &missing_files);

    missing_files
}

fn create_requests(folder: &String, missing_files: Vec<FileInfo>) -> Vec<Request> {
    missing_files
        .iter()
        .map(|file| {
            file.blocks.iter().map(|block| {
                // TODO: check if the request should be sequential number
                let id = rand::random::<i32>().abs();
                let request = Request {
                    id,
                    folder: folder.clone(),
                    name: file.name.clone(),
                    offset: block.offset,
                    size: block.size,
                    hash: block.hash.clone(),
                    from_temporary: false,
                };
                request
            })
        })
        .flatten()
        .collect()
}
