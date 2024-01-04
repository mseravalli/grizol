use crate::core::bep_data_parser::{BepDataParser, CompleteMessage, MAGIC_NUMBER};
use crate::core::bep_state::BepState;
use crate::core::{BepConfig, BepReply, EncodedMessages, UploadStatus};
use crate::device_id::DeviceId;
use crate::storage::StorageManager;
use crate::syncthing;
use chrono::prelude::*;
use chrono_timesource::{ManualTimeSource, TimeSource};
use prost::Message;
use sha2::{Digest, Sha256};
use sqlx::sqlite::SqlitePool;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;
use syncthing::{
    BlockInfo, Close, ClusterConfig, Counter, ErrorCode, FileInfo, Header, Hello, Index,
    IndexUpdate, MessageType, Ping, Request, Response,
};
use tokio::sync::Mutex;

// TODO: maybe change name to something like BepConnectionHandler
pub struct BepProcessor<TS: TimeSource<Utc>> {
    config: BepConfig,
    state: Mutex<BepState<TS>>,
    storage_manager: StorageManager,
}

impl<TS: TimeSource<Utc>> BepProcessor<TS> {
    pub fn new(config: BepConfig, db_pool: SqlitePool, clock: Arc<Mutex<TS>>) -> Self {
        let bep_state = BepState::new(config.clone(), db_pool, clock);

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
            CompleteMessage::DownloadProgress(x) => todo!(),
            _ => todo!(),
        }
    }

    async fn handle_hello(&self, hello: Hello, client_device_id: DeviceId) -> Vec<EncodedMessages> {
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
                        .any(|d| DeviceId::try_from(&d.id).unwrap() == self.config.id)
                })
                .map(|x| x.0)
                .collect();
            debug!("shared folders {:?}", folders_shared_with_me);
            for folder in folders_shared_with_me.into_iter() {
                state.init_index(&folder, &client_device_id).await;
                state.init_index(&folder, &self.config.id).await;
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
        debug!("Received IndexUpdate: {:#?}", &index_update);
        let received_index = Index {
            folder: index_update.folder,
            files: index_update.files,
        };

        let folder = received_index.folder.clone();

        let ems = async move {
            let diff = {
                let state = &mut self.state.lock().await;

                // Update the local index of the connected device
                let other_device_local_index = state
                    .index(&folder, &self.config.id)
                    .await
                    .expect("The local index must exist");
                let indices = vec![received_index];
                let diff = diff_indices(&other_device_local_index, &indices)
                    .expect("Should pass the same folders");
                state
                    .insert_file_info(&folder, &client_device_id, &diff.missing_files)
                    .await;
                state
                    .replace_file_info(&folder, &client_device_id, &diff.conflicting_files, false)
                    .await;

                // Update the local index of the current device
                let local_index = state
                    .index(&folder, &self.config.id)
                    .await
                    .expect("The local index must exist");
                // TODO: ideally we should filter out the local index from all the indices
                let indices = state.indices(Some(&folder), None).await;
                let diff =
                    diff_indices(&local_index, &indices).expect("Should pass the same folders");
                state
                    .insert_file_info(&folder, &self.config.id, &diff.missing_files)
                    .await;
                let file_dests = state
                    .replace_file_info(&folder, &self.config.id, &diff.conflicting_files, true)
                    .await;
                for (orig, dest) in file_dests.iter() {
                    debug!("moved files: {} -> {}", orig, dest);
                    // TODO: actually move the file
                }

                diff
            };

            // TODO: add requests for missing files

            let missing_files_requests = {
                let state = &mut self.state.lock().await;
                let base_req_id = state.load_request_id();
                let requests = create_requests(base_req_id, &folder, diff.missing_files);
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
            // TODO: it's better to send the requests asynchronously independently from
            // receiving the index so that we can recover from crashed etc.
            for request in missing_files_requests.into_iter() {
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
            let diff = {
                let state = &mut self.state.lock().await;

                // Update the local index of the connected device
                state.replace_index(received_index, &client_device_id).await;

                // Update the local index of the current device
                let local_index = state
                    .index(&folder, &self.config.id)
                    .await
                    .expect("The local index must exist");
                // TODO: ideally we should filter out the local index from all the indices
                let indices = state.indices(Some(&folder), None).await;
                let diff =
                    diff_indices(&local_index, &indices).expect("Should pass the same folders");
                state
                    .insert_file_info(&folder, &self.config.id, &diff.missing_files)
                    .await;
                diff
            };

            let missing_files_requests = {
                let state = &mut self.state.lock().await;
                let base_req_id = state.load_request_id();
                let requests = create_requests(base_req_id, &folder, diff.missing_files);
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
            // TODO: it's better to send the requests asynchronously independently from
            // receiving the index so that we can recover from crashed etc.
            for request in missing_files_requests.into_iter() {
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
        client_device_id: DeviceId,
    ) -> Vec<EncodedMessages> {
        debug!("Handling Request");
        debug!("{:?}", request);

        vec![]
    }

    async fn handle_response(
        &self,
        response: Response,
        client_device_id: DeviceId,
    ) -> Vec<EncodedMessages> {
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
                    .update_index_block(&response.id, weak_hash, &client_device_id)
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

    async fn handle_ping(&self, ping: Ping, client_device_id: DeviceId) -> Vec<EncodedMessages> {
        debug!("Handling Ping");
        trace!("{:?}", ping);
        vec![]
    }

    async fn handle_close(&self, close: Close, client_device_id: DeviceId) -> Vec<EncodedMessages> {
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

        let indices: Vec<Index> = self
            .state
            .lock()
            .await
            .indices(None, Some(&self.config.id))
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

#[derive(Debug)]
struct IndexDiff {
    // Files that are not in the index
    missing_files: Vec<FileInfo>,
    // Files that have differences
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

/// Returns a files not present in the current index or that is differnt in other indices.
fn diff_indices(local_index: &Index, remote_indices: &Vec<Index>) -> Result<IndexDiff, String> {
    if remote_indices
        .iter()
        .any(|x| x.folder != local_index.folder)
    {
        return Err(format!("The indices cover different folders"));
    }

    let mut conflicting_files: HashMap<String, Vec<FileInfo>> = Default::default();
    let mut missing_files: HashMap<String, Vec<FileInfo>> = Default::default();

    let local_files: HashMap<&String, &FileInfo> =
        local_index.files.iter().map(|f| (&f.name, f)).collect();

    for remote_index in remote_indices.iter() {
        for remote_file in remote_index.files.iter() {
            if let Some(local_file) = local_files.get(&remote_file.name) {
                if file_has_conflict(local_file, remote_file) {
                    conflicting_files
                        .entry(remote_file.name.clone())
                        .and_modify(|fs| fs.push(remote_file.clone()))
                        .or_insert(vec![remote_file.clone()]);
                }
            } else {
                missing_files
                    .entry(remote_file.name.clone())
                    .and_modify(|fs| fs.push(remote_file.clone()))
                    .or_insert(vec![remote_file.clone()]);
            }
        }
    }

    let diff = IndexDiff {
        missing_files: most_recent(missing_files),
        conflicting_files: most_recent(conflicting_files),
    };

    debug!("Index diff: {:?}", diff);

    Ok(diff)
}

/// Returns the most recent instance for every file
fn most_recent(fs: HashMap<String, Vec<FileInfo>>) -> Vec<FileInfo> {
    // TODO: in case of tie use the device id as per https://docs.syncthing.net/users/syncing.html#conflicting-changes.
    fs.iter()
        .map(|(k, v)| {
            v.iter().max_by(|a, b| {
                a.modified_s
                    .cmp(&b.modified_s)
                    .then(a.modified_ns.cmp(&b.modified_ns))
            })
        })
        .flatten()
        .map(|x| x.clone())
        .collect()
}

/// Checks if a remote file has a timestamp larger than the existing one as specified in
/// https://docs.syncthing.net/users/syncing.html#conflicting-changes.
fn file_has_conflict(local_file: &FileInfo, remote_file: &FileInfo) -> bool {
    trace!(
        "local file: {:?}\nremote file {:?}",
        local_file,
        remote_file
    );
    if local_file.modified_s > remote_file.modified_s {
        return false;
    }

    if local_file.modified_s == remote_file.modified_s
        && local_file.modified_ns > remote_file.modified_ns
    {
        return false;
    }

    if local_file.modified_s == remote_file.modified_s
        && local_file.modified_ns == remote_file.modified_ns
    {
        let mut has_conflict = false;

        has_conflict = has_conflict || local_file.size != remote_file.size;
        has_conflict = has_conflict || local_file.block_size != remote_file.block_size;
        has_conflict = has_conflict || local_file.permissions != remote_file.permissions;
        has_conflict = has_conflict || local_file.modified_by != remote_file.modified_by;
        has_conflict = has_conflict || local_file.deleted != remote_file.deleted;
        has_conflict = has_conflict || local_file.invalid != remote_file.invalid;
        has_conflict = has_conflict || local_file.no_permissions != remote_file.no_permissions;
        has_conflict = has_conflict || local_file.symlink_target != remote_file.symlink_target;

        if !has_conflict {
            for i in 0..local_file.blocks.len() {
                if local_file.blocks[i] != remote_file.blocks[i] {
                    has_conflict = true;
                    break;
                }
            }
        }

        // TODO: add selection of device with largest id  as per https://docs.syncthing.net/users/syncing.html#conflicting-changes.;
        return has_conflict;
    }

    true
}

fn file_modified_in_patch(base_file_info: &FileInfo, patch_file_info: &FileInfo) -> bool {
    let max_patch_version = get_file_max_version(patch_file_info);
    let max_base_version = get_file_max_version(base_file_info);
    max_patch_version.value > max_base_version.value
        || base_file_info.sequence > base_file_info.sequence
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
