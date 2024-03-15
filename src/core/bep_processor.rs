use crate::core::bep_data_parser::{CompleteMessage, MAGIC_NUMBER};
use crate::core::bep_state::{BepState, BlockInfoExt, StorageStatus};
use crate::core::{EncodedMessages, GrizolConfig, UploadStatus};
use crate::device_id::DeviceId;
use crate::grizol::StorageStrategy;
use crate::storage::StorageManager;
use crate::syncthing;
use chrono::prelude::*;
use chrono_timesource::TimeSource;
use prost::Message;
use sha2::{Digest, Sha256};
use std::cmp::Ordering;
use std::collections::HashMap;
use std::sync::Arc;
use syncthing::{
    Close, ClusterConfig, ErrorCode, FileInfo, Header, Hello, Index, IndexUpdate, MessageType,
    Ping, Request, Response,
};
use tokio::sync::Mutex;

// TODO: maybe change name to something like BepConnectionHandler
pub struct BepProcessor<TS: TimeSource<Utc>> {
    config: GrizolConfig,
    state: Arc<Mutex<BepState<TS>>>,
    storage_manager: StorageManager,
}

impl<TS: TimeSource<Utc>> BepProcessor<TS> {
    pub fn new(config: GrizolConfig, state: Arc<Mutex<BepState<TS>>>) -> Self {
        BepProcessor {
            storage_manager: StorageManager::new(config.clone()),
            state,
            config,
        }
    }

    pub async fn handle_complete_message(
        &self,
        complete_message: CompleteMessage,
        client_device_id: DeviceId,
    ) -> Vec<EncodedMessages> {
        match complete_message {
            CompleteMessage::Hello(x) => self.handle_hello(x, client_device_id).await,
            CompleteMessage::ClusterConfig(x) => {
                self.handle_cluster_config(x, client_device_id).await
            }
            CompleteMessage::Index(x) => self.handle_index(x, client_device_id).await,
            CompleteMessage::IndexUpdate(x) => self.handle_index_update(x, client_device_id).await,
            CompleteMessage::Request(x) => self.handle_request(x, client_device_id).await,
            CompleteMessage::Response(x) => self.handle_response(x, client_device_id).await,
            CompleteMessage::Ping(x) => self.handle_ping(x, client_device_id).await,
            CompleteMessage::Close(x) => self.handle_close(x, client_device_id).await,
            CompleteMessage::DownloadProgress(_x) => todo!(),
        }
    }

    async fn handle_hello(
        &self,
        _hello: Hello,
        _client_device_id: DeviceId,
    ) -> Vec<EncodedMessages> {
        debug!("Handling Hello");
        vec![self.hello()]
    }

    async fn handle_cluster_config(
        &self,
        cluster_config: ClusterConfig,
        client_device_id: DeviceId,
    ) -> Vec<EncodedMessages> {
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
                        .any(|d| DeviceId::try_from(&d.id).unwrap() == self.config.local_device_id)
                })
                .map(|x| x.0)
                .collect();
            debug!("shared folders {:?}", folders_shared_with_me);
            for folder in folders_shared_with_me.into_iter() {
                state.init_index(&folder, &client_device_id).await;
                state
                    .init_index(&folder, &self.config.local_device_id)
                    .await;
            }
        }

        vec![self.cluster_config().await, self.index().await]
    }

    async fn handle_index_update(
        &self,
        index_update: IndexUpdate,
        client_device_id: DeviceId,
    ) -> Vec<EncodedMessages> {
        debug!("Handling IndexUpdate");
        trace!("Received IndexUpdate: {:#?}", &index_update);
        let received_index = Index {
            folder: index_update.folder,
            files: index_update.files,
        };

        let folder = received_index.folder.clone();

        let ems = async move {
            let unstored_blocks = {
                let state = &mut self.state.lock().await;

                // Update the local index of the connected device
                let other_device_local_index = state
                    .index(&folder, &client_device_id)
                    .await
                    .expect("The local index must exist");
                let diff = diff_indices(&folder, &received_index, &other_device_local_index)
                    .expect("Should pass the same folders");
                state
                    .rm_replace_file_info(&folder, &client_device_id, &diff.newly_added_files)
                    .await;

                // Update the local index of the current device
                let local_index = state
                    .index(&folder, &self.config.local_device_id)
                    .await
                    .expect("The index must exist");
                let other_device_local_index = state
                    .index(&folder, &client_device_id)
                    .await
                    .expect("The index must exist");
                let diff = diff_indices(&folder, &other_device_local_index, &local_index)
                    .expect("Should pass the same folders");
                state
                    .rm_replace_file_info(
                        &folder,
                        &self.config.local_device_id,
                        &diff.newly_added_files,
                    )
                    .await;
                // let file_dests = state
                //     .mv_replace_file_info(
                //         &folder,
                //         &self.config.id,
                //         &diff.newly_added_conflicting_files,
                //     )
                //     .await;
                // for (orig, dest) in file_dests.iter() {
                //     self.storage_manager.move_file(orig, dest).await;
                //     debug!("moved file: {} -> {}", orig, dest);
                // }

                state
                    .blocks_info(&self.config.local_device_id, StorageStatus::NotStored)
                    .await
            };

            let file_requests = {
                let state = &mut self.state.lock().await;
                let base_req_id = state.load_request_id();
                let requests = requests_from_blocks(base_req_id, unstored_blocks);
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

            for request in file_requests.into_iter() {
                ems.append(encode_message(header.clone(), request).unwrap());
            }

            ems
        }
        .await;

        vec![ems]
    }

    async fn handle_index(
        &self,
        received_index: Index,
        client_device_id: DeviceId,
    ) -> Vec<EncodedMessages> {
        debug!("Handling Index");
        trace!("Received Index: {:#?}", &received_index);
        let folder = received_index.folder.clone();

        let ems = async move {
            let unstored_blocks = {
                let state = &mut self.state.lock().await;

                // Update the local index of the connected device
                state
                    .replace_index(received_index.clone(), &client_device_id)
                    .await;

                // Update the local index of the current device
                let local_index = state
                    .index(&folder, &self.config.local_device_id)
                    .await
                    .expect("The local index must exist");
                // TODO: ideally we should filter out the local index from all the indices
                let _indices = state.indices(Some(&folder), None).await;
                let diff = diff_indices(&folder, &received_index, &local_index)
                    .expect("Should pass the same folders");
                state
                    .insert_file_info(
                        &folder,
                        &self.config.local_device_id,
                        &diff.newly_added_files,
                    )
                    .await;

                state
                    .blocks_info(&self.config.local_device_id, StorageStatus::NotStored)
                    .await
            };

            let file_requests = {
                let state = &mut self.state.lock().await;
                let base_req_id = state.load_request_id();
                let requests = requests_from_blocks(base_req_id, unstored_blocks);
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

            for request in file_requests.into_iter() {
                ems.append(encode_message(header.clone(), request).unwrap());
            }

            ems
        }
        .await;

        vec![ems]
    }

    async fn handle_request(
        &self,
        request: Request,
        _client_device_id: DeviceId,
    ) -> Vec<EncodedMessages> {
        debug!("Handling Request");
        debug!("{:?}", request);

        vec![]
    }

    async fn handle_response(
        &self,
        response: Response,
        _client_device_id: DeviceId,
    ) -> Vec<EncodedMessages> {
        debug!("Handling Response");
        debug!(
            "Received Response: {:?}, {}",
            response.id,
            response.data.len()
        );

        if ErrorCode::NoError
            != ErrorCode::try_from(response.code).expect("Enum value must be valid")
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
                if let Err(e) = check_data(&response.data, &request.hash) {
                    panic!("Wrong data received: {}", e);
                }
                let weak_hash = compute_weak_hash(&response.data);
                // TODO: check the weak hash against the hashes in other devices.
                let file_size: u64 = state
                    .file(&request.folder, self.config.local_device_id, &request.name)
                    .await
                    .unwrap_or_else(|| {
                        panic!(
                            "Requesting a file not in the local index: {}",
                            &request.name
                        )
                    })
                    .size
                    .try_into()
                    .unwrap();
                self.storage_manager
                    .store_block(response.data, request, file_size)
                    .await
                    .expect("Error while storing the data");
                let upload_status = state
                    .update_block_storage_state(
                        &response.id,
                        weak_hash,
                        &self.config.local_device_id,
                    )
                    .await
                    .expect("It was not possible to update the index");

                // TODO: test behaviour with directory
                if upload_status == UploadStatus::AllBlocks {
                    if self.config.storage_strategy == StorageStrategy::Remote
                        || self.config.storage_strategy == StorageStrategy::LocalRemote
                    {
                        let locations = self
                            .storage_manager
                            .cp_to_remote(&request.folder, &request.name)
                            .await
                            .expect("Error occured when uploading file");
                        state
                            .update_file_locations(
                                &request.folder,
                                &self.config.local_device_id,
                                &request.name,
                                locations,
                            )
                            .await;
                    }

                    if self.config.storage_strategy == StorageStrategy::Remote {
                        state
                            .remove_file_locations(
                                &request.folder,
                                &self.config.local_device_id,
                                &request.name,
                                vec!["local".to_string()],
                            )
                            .await;
                        self.storage_manager
                            .rm_local_file(&request.folder, &request.name)
                            .await
                            .unwrap_or_else(|_| {
                                panic!(
                                    "Error occured when removing the local file {}/{}",
                                    &request.folder, &request.name
                                )
                            });
                    }
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

    async fn handle_ping(&self, ping: Ping, _client_device_id: DeviceId) -> Vec<EncodedMessages> {
        debug!("Handling Ping");
        trace!("{:?}", ping);
        vec![]
    }

    async fn handle_close(
        &self,
        close: Close,
        _client_device_id: DeviceId,
    ) -> Vec<EncodedMessages> {
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
            .chain(MAGIC_NUMBER)
            .chain(message_len.to_be_bytes())
            .chain(message_bytes)
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

        let indices: Vec<Index> = self
            .state
            .lock()
            .await
            .indices(None, Some(&self.config.local_device_id))
            .await;
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
        .chain(header_len.to_be_bytes())
        .chain(header_bytes)
        .chain(message_len.to_be_bytes())
        .chain(message_bytes)
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

#[derive(Debug)]
struct IndexDiff {
    // Files that are not in the index
    newly_added_files: Vec<FileInfo>,
    _newly_added_conflicting_files: Vec<FileInfo>,
    _existing_conflicting_files: Vec<FileInfo>,
}

fn diff_indices(
    folder: &str,
    added_index: &Index,
    existing_index: &Index,
) -> Result<IndexDiff, String> {
    if added_index.folder != folder && existing_index.folder != folder {
        return Err("The indices cover different folders".to_string());
    }

    let existing_files: HashMap<&String, &FileInfo> =
        existing_index.files.iter().map(|f| (&f.name, f)).collect();

    let mut newly_added_files: Vec<FileInfo> = Default::default();
    let mut newly_added_conflicting_files: Vec<FileInfo> = Default::default();
    let mut existing_conflicting_files: Vec<FileInfo> = Default::default();

    for newly_added_file in added_index.files.iter() {
        if let Some(existing_file) = existing_files.get(&newly_added_file.name) {
            match partial_cmp_file_version(existing_file, newly_added_file) {
                Some(Ordering::Less) => {
                    newly_added_files.push(newly_added_file.clone());
                }
                Some(Ordering::Equal) => {}
                Some(Ordering::Greater) => { /* the existing file is newer than the added file*/ }
                None => match partial_cmp_file_timestamp(existing_file, newly_added_file) {
                    Ordering::Less => {
                        newly_added_conflicting_files.push(newly_added_file.clone());
                    }
                    Ordering::Greater => {
                        existing_conflicting_files.push(newly_added_file.clone());
                    }
                    Ordering::Equal => {
                        panic!("It should mean we are comparing the same file, which should not happen");
                    }
                },
            };
        } else {
            newly_added_files.push(newly_added_file.clone());
        }
    }

    let diff = IndexDiff {
        newly_added_files,
        _newly_added_conflicting_files: newly_added_conflicting_files,
        _existing_conflicting_files: existing_conflicting_files,
    };

    debug!("Diff: {:?}", &diff);

    Ok(diff)
}

/// Compares [a] and [b] based on the version according to https://docs.syncthing.net/specs/bep-v1.html#fields-fileinfo-message.
fn partial_cmp_file_version(a: &FileInfo, b: &FileInfo) -> Option<Ordering> {
    trace!("partial_cmp_file_version\na: {:?}\nb: {:?}", a, b);
    let max_counter_a = a
        .version
        .as_ref()
        .unwrap()
        .counters
        .iter()
        .max_by_key(|c| c.value)
        .unwrap();
    let max_counter_b = b
        .version
        .as_ref()
        .unwrap()
        .counters
        .iter()
        .max_by_key(|c| c.value)
        .unwrap();

    let res = if max_counter_a.value < max_counter_b.value {
        Some(Ordering::Less)
    } else if max_counter_a.value > max_counter_b.value {
        Some(Ordering::Greater)
    } else if max_counter_a.value == max_counter_b.value && max_counter_a.id == max_counter_b.id {
        Some(Ordering::Equal)
    } else {
        None
    };

    trace!("partial_cmp_file_version: {:?}", res);
    res
}

/// Compares [a] and [b] based on the timestamps.
/// This is used in case of conflicts.
fn partial_cmp_file_timestamp(a: &FileInfo, b: &FileInfo) -> Ordering {
    trace!("a: {:?}\nb {:?}", a, b);

    if a.modified_s < b.modified_s || a.modified_s == b.modified_s && a.modified_ns < b.modified_ns
    {
        Ordering::Less
    } else if a.modified_s > b.modified_s
        || a.modified_s == b.modified_s && a.modified_ns > b.modified_ns
    {
        return Ordering::Greater;
    } else {
        let max_counter_a = a
            .version
            .as_ref()
            .unwrap()
            .counters
            .iter()
            .max_by_key(|c| c.value)
            .unwrap();
        let max_counter_b = b
            .version
            .as_ref()
            .unwrap()
            .counters
            .iter()
            .max_by_key(|c| c.value)
            .unwrap();

        return max_counter_a.value.cmp(&max_counter_b.value);
    }
}

// TODO: maybe it might make sense to pass an iterator to the function to generate the ids so that
// we can be more flexible? Check if that's an overkill.
fn requests_from_blocks(base_request_id: i32, block_ids: Vec<BlockInfoExt>) -> Vec<Request> {
    let mut request_id = base_request_id;
    let mut res: Vec<Request> = vec![];
    for block_id in block_ids.into_iter() {
        request_id += 1;
        debug!(
            "Outgoing request {} with hash {:?}",
            request_id, &block_id.hash
        );
        let request = Request {
            id: request_id,
            folder: block_id.folder,
            name: block_id.name,
            offset: block_id.offset,
            size: block_id.size,
            hash: block_id.hash.clone(),
            from_temporary: false,
        };
        res.push(request)
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
        Err(format!(
            "Hash of incoming data does not match requested data, expected {:?}, got {:?}",
            hash, data_hash
        ))
    }
}

fn compute_weak_hash(_data: &[u8]) -> u32 {
    0
}
