use crate::core::bep_data_parser::{BepDataParser, CompleteMessage, MAGIC_NUMBER};
use crate::core::bep_state::BepState;
use crate::core::{BepConfig, BepReply, EncodedMessages, UploadStatus};
use crate::device_id::DeviceId;
use crate::storage::StorageManager;
use crate::syncthing;
use prost::Message;
use sha2::{Digest, Sha256};
use sqlx::sqlite::SqlitePool;
use std::collections::{BTreeMap, HashMap, HashSet};
use syncthing::{
    BlockInfo, Close, ClusterConfig, Counter, ErrorCode, FileInfo, Header, Hello, Index,
    IndexUpdate, MessageType, Ping, Request, Response,
};
use tokio::sync::Mutex;

// TODO: maybe change name to something like BepConnectionHandler
pub struct BepProcessor {
    config: BepConfig,
    state: Mutex<BepState>,
    storage_manager: StorageManager,
}

impl BepProcessor {
    pub fn new(config: BepConfig, db_pool: SqlitePool) -> Self {
        let bep_state = BepState::new(config.clone(), db_pool);

        let base_dir = config.base_dir.clone();
        BepProcessor {
            config,
            state: Mutex::new(bep_state),
            storage_manager: StorageManager::new(base_dir),
        }
    }

    pub async fn handle_complete_message(
        &self,
        complete_message: CompleteMessage,
    ) -> Vec<EncodedMessages> {
        match complete_message {
            CompleteMessage::Hello(x) => self.handle_hello(x).await,
            CompleteMessage::ClusterConfig(x) => self.handle_cluster_config(x).await,
            CompleteMessage::Index(x) => self.handle_index(x).await,
            CompleteMessage::IndexUpdate(x) => self.handle_index_update(x).await,
            CompleteMessage::Request(x) => self.handle_request(x).await,
            CompleteMessage::Response(x) => self.handle_response(x).await,
            CompleteMessage::Ping(x) => self.handle_ping(x).await,
            CompleteMessage::Close(x) => self.handle_close(x).await,
            CompleteMessage::DownloadProgress(x) => todo!(),
            _ => todo!(),
        }
    }

    async fn handle_hello(&self, hello: Hello) -> Vec<EncodedMessages> {
        debug!("Handling Hello");
        vec![self.hello()]
    }

    async fn handle_cluster_config(&self, cluster_config: ClusterConfig) -> Vec<EncodedMessages> {
        debug!("Handling Cluster Config");
        trace!("Received Cluster Config: {:#?}", &cluster_config);
        {
            let mut state = self.state.lock().await;
            state.update_cluster_config(&cluster_config).await;

            let folders_shared_with_me: Vec<String> = cluster_config
                .folders
                .iter()
                .map(|f| (f.id.clone(), f.devices.clone()))
                .filter(|x| {
                    x.1.iter()
                        .any(|d| DeviceId::try_from(&d.id).unwrap() == self.config.id)
                })
                .map(|x| x.0)
                .collect();
            debug!("shared folders {:?}", folders_shared_with_me);
            for folder in folders_shared_with_me.into_iter() {
                state.init_index(&folder).await;
            }
        }

        vec![self.cluster_config().await, self.index().await]
    }

    async fn handle_index_update(&self, index_update: IndexUpdate) -> Vec<EncodedMessages> {
        debug!("Handling Index Update");
        let index = Index {
            folder: index_update.folder,
            files: index_update.files,
        };
        self.handle_index(index).await
    }

    async fn handle_index(&self, received_index: Index) -> Vec<EncodedMessages> {
        debug!("Handling Index");
        trace!("Received Index: {:#?}", &received_index);

        let ems = async move {
            let missing_files = {
                let state = &mut self.state.lock().await;

                let local_index: Index = state
                    .index(&received_index.folder, &self.config.id)
                    .await
                    .expect(&format!(
                        "The index for local device {} must be present",
                        &self.config.id
                    ));

                let index_diff = diff_indices(&local_index, &received_index);

                state
                    .add_missing_files_to_local_index(
                        &received_index.folder,
                        &index_diff.missing_files,
                    )
                    .await;

                state.insert_conflicting_files(
                    received_index.folder.to_string(),
                    index_diff.conflicting_files,
                );

                index_diff.missing_files
            };

            let requests = {
                let state = &mut self.state.lock().await;
                let base_req_id = state.load_request_id();
                let requests = create_requests(base_req_id, &received_index.folder, missing_files);
                let max_req_id = requests.iter().map(|x| x.id).max().unwrap_or(base_req_id);
                state.fetch_add_request_id(max_req_id - base_req_id);
                state.insert_requests(&requests);
                requests
            };

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

        vec![ems.await]
    }

    async fn handle_request(&self, request: Request) -> Vec<EncodedMessages> {
        debug!("Handling Request");
        debug!("{:?}", request);

        vec![]
    }

    async fn handle_response(&self, response: Response) -> Vec<EncodedMessages> {
        debug!("Handling Response");
        debug!(
            "Received Response: {:?}, {}",
            response.id,
            response.data.len()
        );

        if ErrorCode::NoError
            != ErrorCode::from_i32(response.code).expect("Enum value must be valid")
        {
            warn!(
                "Request with id {} genereated an error: {:?} ",
                response.id, response.code
            );
        }

        // TODO: put this in an external method and use ? with the results.
        let res = async move {
            let state = &mut self.state.lock().await;
            debug!("Got the lock");
            if let Some(request) = state.get_request(&response.id) {
                check_data(&response.data, &request.hash)
                    .expect("The hash does not match the data");
                let weak_hash = compute_weak_hash(&response.data);
                // TODO: check the weak hash against the hashes in other devices.
                let file_size: u64 = state
                    .file_from_local_index(&request.folder, &request.name)
                    .await
                    .expect(&format!(
                        "Requesting a file not in the index: {}",
                        &request.name
                    ))
                    .size
                    .try_into()
                    .unwrap();
                self.storage_manager
                    .store_block(response.data, request, file_size)
                    .await
                    .expect("Error while storing the data");
                let upload_status = state
                    .update_index_block(&response.id, weak_hash)
                    .await
                    .expect("It was not possible to update the index");

                if upload_status == UploadStatus::AllBlocks {
                    // TODO: move the file to a remote backend
                    debug!("Stored whole file: {}", &request.name);
                }

                state.remove_request(&response.id);
            } else {
                error!(
                    "Response with id {} does not have a corresponding request",
                    response.id
                );
            }

            EncodedMessages::empty()
        };

        vec![res.await]
    }

    async fn handle_ping(&self, ping: Ping) -> Vec<EncodedMessages> {
        debug!("Handling Ping");
        trace!("{:?}", ping);
        vec![]
    }

    async fn handle_close(&self, close: Close) -> Vec<EncodedMessages> {
        debug!("Handling Close");
        trace!("{:?}", close);
        vec![]
    }

    fn hello(&self) -> EncodedMessages {
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

        let cluster_config = self
            .state
            .lock()
            .await
            .cluster_config()
            .await
            .expect("something went wrong when restoring");

        debug!("Sending Cluster Config: {:?}", &cluster_config);
        encode_message(header, cluster_config).unwrap()
    }

    async fn index(&self) -> EncodedMessages {
        let header = Header {
            compression: 0,
            r#type: MessageType::Index.into(),
        };

        let indices: Vec<Index> = self.state.lock().await.indices(&self.config.id).await;
        trace!("Sending Index, {:?}", &indices);

        let mut res = EncodedMessages::empty();
        for index in indices.into_iter() {
            res.append(encode_message(header.clone(), index).unwrap());
        }

        debug!("Sending Index");
        res
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

struct IndexDiff {
    missing_files: Vec<FileInfo>,
    conflicting_files: Vec<FileInfo>,
}

fn get_file_max_version(file: &FileInfo) -> &Counter {
    file.version
        .as_ref()
        .expect("The file must be versioned")
        .counters
        .iter()
        .max_by_key(|c| c.value)
        .expect("The file must be versioned")
}

/// Returns a set of files present in [patch] that are not present in [base]. The returned files
/// cointain only the blocks not present in [base].
fn diff_indices(base: &Index, patch: &Index) -> IndexDiff {
    let mut conflicting_files: Vec<FileInfo> = vec![];
    let mut missing_files: Vec<FileInfo> = vec![];
    let base_file_map: HashMap<&String, &FileInfo> =
        base.files.iter().map(|x| (&x.name, x)).collect();

    // TODO: remove this, we can just use the iterator in the for loop
    let patch_file_map: HashMap<&String, &FileInfo> =
        patch.files.iter().map(|x| (&x.name, x)).collect();

    for (name, patch_file_info) in patch_file_map.into_iter() {
        if let Some(base_file_info) = base_file_map.get(name) {
            let max_patch_version = get_file_max_version(patch_file_info);
            let max_base_version = get_file_max_version(base_file_info);
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

    IndexDiff {
        missing_files,
        conflicting_files,
    }
}

// TODO: maybe it might make sense to pass an iterator to the function to generate the ids so that
// we can be more flexible? Check if that's an overkill.
fn create_requests(
    base_request_id: i32,
    folder: &String,
    missing_files: Vec<FileInfo>,
) -> Vec<Request> {
    let mut request_id = base_request_id;
    let mut res: Vec<Request> = vec![];
    for file in missing_files.iter() {
        for block in file.blocks.iter() {
            request_id += 1;
            let request = Request {
                id: request_id,
                folder: folder.clone(),
                name: file.name.clone(),
                offset: block.offset,
                size: block.size,
                hash: block.hash.clone(),
                from_temporary: false,
            };
            res.push(request)
        }
    }
    res
}

fn check_data(data: &[u8], hash: &[u8]) -> Result<(), String> {
    debug!("Start checking the hashes");
    let mut hasher_sha256 = Sha256::new();
    hasher_sha256.update(data);

    let data_hash = hasher_sha256.finalize().to_vec();
    if data_hash == hash {
        Ok(())
    } else {
        Err(format!("Hash of incoming data does not match request data"))
    }
}

fn compute_weak_hash(data: &[u8]) -> u32 {
    0
}
