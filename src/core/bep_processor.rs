use crate::core::bep_data_parser::{CompleteMessage};
use crate::core::bep_state::{BepState, BlockInfoExt, StorageStatus};
use crate::core::{GrizolConfig, UploadStatus};
use crate::device_id::DeviceId;
use crate::grizol::StorageStrategy;
use crate::storage::StorageManager;
use crate::syncthing;
use chrono::prelude::*;
use chrono_timesource::TimeSource;

use sha2::{Digest, Sha256};
use std::cmp::Ordering;
use std::collections::HashMap;
use std::error::Error;
use std::fmt;
use std::sync::Arc;
use syncthing::{
    Close, ClusterConfig, ErrorCode, FileInfo, Header, Hello, Index, IndexUpdate, MessageType,
    Ping, Request, Response,
};
use tokio::sync::Mutex;

use super::GrizolEvent;

// TODO: maybe change name to something like BepConnectionHandler
pub struct BepProcessor<TS: TimeSource<Utc>> {
    config: GrizolConfig,
    state: Arc<Mutex<BepState<TS>>>,
    storage_manager: StorageManager,
}

#[derive(Debug)]
pub enum BepProcessorError {
    NoAssociatedReqeust(String),
    WrongDataInResponse(String),
    ErrorInResponse(String),
    FileNotInIndex(String),
    StorageError(String),
    StateError(String),
}

impl fmt::Display for BepProcessorError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let msg = match self {
            Self::NoAssociatedReqeust(m) => format!("NoAssociatedReqeust: {}", m),
            Self::WrongDataInResponse(m) => format!("WrongDataInResponse: {}", m),
            Self::ErrorInResponse(m) => format!("ErrorInResponse: {}", m),
            Self::FileNotInIndex(m) => format!("FileNotInIndex: {}", m),
            Self::StorageError(m) => format!("StorageError: {}", m),
            Self::StateError(m) => format!("StateError: {}", m),
        };
        write!(f, "{}", msg)
    }
}
impl Error for BepProcessorError {}

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
    ) -> Vec<Result<GrizolEvent, BepProcessorError>> {
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
    ) -> Vec<Result<GrizolEvent, BepProcessorError>> {
        debug!("Handling Hello");

        let hello = Hello {
            device_name: self.config.name.clone(),
            client_name: env!("CARGO_PKG_NAME").to_string(),
            client_version: env!("CARGO_PKG_VERSION").to_string(),
        };

        vec![Ok(GrizolEvent::Message(CompleteMessage::Hello(hello)))]
    }

    async fn handle_cluster_config(
        &self,
        cluster_config: ClusterConfig,
        client_device_id: DeviceId,
    ) -> Vec<Result<GrizolEvent, BepProcessorError>> {
        debug!("Handling Cluster Config");
        trace!("Received Cluster Config: {:#?}", &cluster_config);
        {
            let state = self.state.lock().await;
            state
                .update_cluster_config(&cluster_config, &self.config.local_device_id)
                .await;

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

        let indices: Vec<Result<GrizolEvent, BepProcessorError>> = self
            .indices(client_device_id)
            .await
            .into_iter()
            .map(|x| Ok(GrizolEvent::Message(x)))
            .collect();
        vec![Ok(GrizolEvent::Message(
            self.cluster_config(client_device_id).await,
        ))]
        .into_iter()
        .chain(indices)
        .collect()
    }

    async fn handle_index_update(
        &self,
        index_update: IndexUpdate,
        client_device_id: DeviceId,
    ) -> Vec<Result<GrizolEvent, BepProcessorError>> {
        debug!("Handling IndexUpdate");
        trace!("Received IndexUpdate: {:#?}", &index_update);
        let received_index = Index {
            folder: index_update.folder,
            files: index_update.files,
        };

        let folder = received_index.folder.clone();

        let _file_requests = async move {
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
                    .rm_replace_file_info(&folder, &client_device_id, diff.newly_added_files)
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
                        diff.newly_added_files,
                    )
                    .await;

                state
                    .blocks_info(&self.config.local_device_id, StorageStatus::NotStored)
                    .await
            };

            let _file_requests = {
                let state = &mut self.state.lock().await;
                store_outgoing_requests(state, unstored_blocks).await
            };
        }
        .await;

        debug!("Finished handling IndexUpdate");

        vec![Ok(GrizolEvent::RequestCreated)]
    }

    async fn handle_index(
        &self,
        received_index: Index,
        client_device_id: DeviceId,
    ) -> Vec<Result<GrizolEvent, BepProcessorError>> {
        debug!("Handling Index");
        trace!("Received Index: {:#?}", &received_index);
        let folder = received_index.folder.clone();

        async move {
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
                let diff = diff_indices(&folder, &received_index, &local_index)
                    .expect("Should pass the same folders");
                state
                    .insert_file_info(
                        &folder,
                        &self.config.local_device_id,
                        diff.newly_added_files,
                    )
                    .await;

                state
                    .blocks_info(&self.config.local_device_id, StorageStatus::NotStored)
                    .await
            };

            let _file_requests = {
                let state = &mut self.state.lock().await;
                store_outgoing_requests(state, unstored_blocks).await
            };
        }
        .await;

        vec![Ok(GrizolEvent::RequestCreated)]
    }

    async fn handle_request(
        &self,
        request: Request,
        _client_device_id: DeviceId,
    ) -> Vec<Result<GrizolEvent, BepProcessorError>> {
        debug!("Handling Request");
        debug!("{:?}", request);

        vec![]
    }

    async fn handle_response(
        &self,
        response: Response,
        _client_device_id: DeviceId,
    ) -> Vec<Result<GrizolEvent, BepProcessorError>> {
        debug!("Handling Response");
        debug!(
            "Received Response: {:?}, {}",
            response.id,
            response.data.len()
        );

        if ErrorCode::NoError
            != ErrorCode::try_from(response.code).expect("Enum value must be valid")
        {
            let msg = format!(
                "Request with id {} genereated response containing error: {:?} ",
                response.id, response.code
            );
            return vec![Err(BepProcessorError::ErrorInResponse(msg))];
        }

        let res = self.store_response_data(response).await;

        vec![res]
    }

    async fn handle_ping(
        &self,
        ping: Ping,
        _client_device_id: DeviceId,
    ) -> Vec<Result<GrizolEvent, BepProcessorError>> {
        debug!("Handling Ping");
        trace!("{:?}", ping);
        vec![]
    }

    async fn handle_close(
        &self,
        close: Close,
        _client_device_id: DeviceId,
    ) -> Vec<Result<GrizolEvent, BepProcessorError>> {
        debug!("Handling Close");
        trace!("{:?}", close);
        vec![]
    }

    // TODO: read this from the last state and add additional information from the config.
    async fn cluster_config(&self, client_device_id: DeviceId) -> CompleteMessage {
        let _header = Header {
            compression: 0,
            r#type: MessageType::ClusterConfig.into(),
        };

        let cluster_config = self
            .state
            .lock()
            .await
            .cluster_config(self.config.local_device_id, client_device_id)
            .await
            .expect("something went wrong when restoring");

        debug!("Sending Cluster Config: {:?}", &cluster_config);
        CompleteMessage::ClusterConfig(cluster_config)
    }

    async fn indices(&self, client_device_id: DeviceId) -> Vec<CompleteMessage> {
        let indices: Vec<Index> = self
            .state
            .lock()
            .await
            .indices(None, Some(&self.config.local_device_id), client_device_id)
            .await;

        debug!("Sending Index");
        trace!("Sending Index, {:?}", &indices);

        indices
            .into_iter()
            .map(|i| CompleteMessage::Index(i))
            .collect()
    }

    async fn store_response_data(
        &self,
        response: Response,
    ) -> Result<GrizolEvent, BepProcessorError> {
        let request_opt: Option<Request> = {
            let state = &mut self.state.lock().await;
            state.get_request(&response.id).map(|x| x.clone())
        };
        if let Some(request) = request_opt {
            if let Err(e) = check_data(&response.data, &request.hash) {
                let msg = format!("Wrong data received for request {:?}: {}", &request, e);
                return Err(BepProcessorError::WrongDataInResponse(msg));
            }

            let weak_hash = compute_weak_hash(&response.data);
            let file_size: u64 = {
                let state = &mut self.state.lock().await;
                // TODO: check the weak hash against the hashes in other devices.
                state
                    .file_info(&request.folder, self.config.local_device_id, &request.name)
                    .await
                    .ok_or({
                        let msg = format!(
                            "Requesting a file not in the local index: {}",
                            &request.name
                        );
                        BepProcessorError::FileNotInIndex(msg)
                    })?
                    .size
                    .try_into()
                    .unwrap()
            };
            self.storage_manager
                .store_block(response.data, &request, file_size)
                .await
                .map_err(|e| BepProcessorError::StorageError(e.to_string()))?;
            let upload_status = {
                let state = &mut self.state.lock().await;
                state
                    .update_block_storage_state(
                        &response.id,
                        weak_hash,
                        &self.config.local_device_id,
                    )
                    .await
                    .map_err(|e| BepProcessorError::StateError(e.to_string()))?
            };

            // TODO: test behaviour with directory
            if upload_status == UploadStatus::AllBlocks {
                if self.config.storage_strategy == StorageStrategy::Remote
                    || self.config.storage_strategy == StorageStrategy::LocalRemote
                {
                    let locations = self
                        .storage_manager
                        .cp_local_to_remote(&request.folder, &request.name)
                        .await
                        .map_err(|e| BepProcessorError::StorageError(e.to_string()))?;

                    {
                        let state = &mut self.state.lock().await;
                        state
                            .update_file_locations(
                                &request.folder,
                                &self.config.local_device_id,
                                &request.name,
                                locations,
                            )
                            .await;
                    }
                }

                if self.config.storage_strategy == StorageStrategy::Remote {
                    {
                        let state = &mut self.state.lock().await;
                        state
                            .remove_file_locations(
                                &request.folder,
                                &self.config.local_device_id,
                                &request.name,
                                vec!["local".to_string()],
                            )
                            .await;
                    }

                    self.storage_manager
                        .rm_local_file(&request.folder, &request.name)
                        .await
                        .map_err(|e| BepProcessorError::StorageError(e.to_string()))?;
                }
                debug!("Stored whole file: {}", &request.name);
            }

            {
                let state = &mut self.state.lock().await;
                state.remove_request(&response.id);
            }
        } else {
            let msg = format!(
                "Response with id {} does not have a corresponding request",
                response.id
            );
            return Err(BepProcessorError::NoAssociatedReqeust(msg));
        }

        Ok(GrizolEvent::RequestProcessed)
    }

    /// Enqueues the requests and up to []
    pub async fn throttled_requests(&self) -> Vec<Request> {
        let mut state = self.state.lock().await;
        state.update_pending_requests(
            self.config.max_pending_requests,
            self.config.max_total_pending_requests_size,
        )
    }
}

async fn store_outgoing_requests<TS: TimeSource<Utc>>(
    state: &mut BepState<TS>,
    unstored_blocks: Vec<BlockInfoExt>,
) -> Vec<Request> {
    let base_req_id = state.load_request_id();
    let requests = requests_from_blocks(base_req_id, unstored_blocks);
    let max_req_id = requests.iter().map(|x| x.id).max().unwrap_or(base_req_id);
    state.fetch_add_request_id(max_req_id - base_req_id);
    state.insert_requests(requests)
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

    trace!("Diff: {:?}", &diff);

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
        trace!(
            "Creating outgoing request {}, for file {}, with hash {:?}",
            request_id,
            &block_id.name,
            &block_id.hash
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
