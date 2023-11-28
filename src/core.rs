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

#[derive(Debug)]
struct BepState {
    config: BepConfig,
    db_pool: SqlitePool,
    indices: HashMap<String, HashMap<DeviceId, syncthing::Index>>,
    cluster: ClusterConfig,
    sequence: u64,
    request_id: i32,
    conflicting_files: HashMap<(String, String), FileInfo>,
    outgoing_requests: HashMap<i32, Request>,
}

// async fn run_transaction(db_pool: &SqlitePool, queries: Vec<String>) -> Result<(), sqlx::Error> {
//     sqlx::query!("BEGIN TRANSACTION;").execute(db_pool).await?;
//     for q in queries.into_iter() {
//         q.execute(db_pool).await?;
//     }
//     sqlx::query!("END TRANSACTION;").execute(db_pool).await?;
//     Ok(())
// }

impl BepState {
    pub fn new(config: BepConfig, db_pool: SqlitePool) -> Self {
        BepState {
            config,
            db_pool,
            indices: Default::default(),
            cluster: ClusterConfig {
                folders: Default::default(),
            },
            sequence: 0,
            request_id: 0,
            conflicting_files: Default::default(),
            outgoing_requests: Default::default(),
        }
    }
    async fn init_device(&self, folder: &str) {
        todo!()
    }

    async fn init_index(&self, folder: &str) {
        let device_id = self.config.id.to_string();
        let insert_res = sqlx::query!(
            "
        INSERT INTO bep_index (
            device,
            folder
        )
        VALUES (?, ?)
        ON CONFLICT(folder, device) DO UPDATE SET
            device = excluded.device,
            folder = excluded.folder
        ",
            device_id,
            folder,
        )
        .execute(&self.db_pool)
        .await
        .expect("Failed to execute query");
    }

    async fn update_cluster_config(&mut self, other: &ClusterConfig) -> Vec<SqliteQueryResult> {
        let mut insert_results: Vec<SqliteQueryResult> = vec![];

        sqlx::query!("BEGIN TRANSACTION;")
            .execute(&self.db_pool)
            .await
            .unwrap();

        for other_folder in other.folders.iter() {
            // FIXME: recover from error and rollback transaction
            let insert_res = sqlx::query!(
                "
                INSERT INTO bep_folders (
                    id,
                    label,
                    read_only,
                    ignore_permissions,
                    ignore_delete,
                    disable_temp_indexes,
                    paused
                )
                VALUES (?, ?, ?, ?, ?, ?, ?)
                ON CONFLICT(id) DO UPDATE SET
                    id                   = excluded.id,
                    label                = excluded.label,
                    read_only            = excluded.read_only,
                    ignore_permissions   = excluded.ignore_permissions,
                    ignore_delete        = excluded.ignore_delete,
                    disable_temp_indexes = excluded.disable_temp_indexes,
                    paused               = excluded.paused
                ",
                other_folder.id,
                other_folder.label,
                other_folder.read_only,
                other_folder.ignore_permissions,
                other_folder.ignore_delete,
                other_folder.disable_temp_indexes,
                other_folder.paused,
            )
            .execute(&self.db_pool)
            .await
            .expect("Failed to execute query");

            for device in other_folder.devices.iter() {
                // TODO: handle the case about the current device.
                let index_id: Vec<u8> = device.index_id.to_be_bytes().into();
                trace!(
                    "Inserting index id {:?} as {:?}",
                    &device.index_id,
                    &index_id
                );
                let device_addresses = device.addresses.join(",");
                let device_id = DeviceId::try_from(&device.id)
                    .expect("Wrong device id format")
                    .to_string();

                let insert_res = sqlx::query!(
                    "
                    INSERT INTO bep_devices (
                        folder                     ,
                        id                         ,
                        name                       ,
                        addresses                  ,
                        compression                ,
                        cert_name                  ,
                        max_sequence               ,
                        introducer                 ,
                        index_id                   ,
                        skip_introduction_removals ,
                        encryption_password_token  
                    )
                    VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
                    ON CONFLICT(id) DO UPDATE SET
                        folder                     = excluded.folder                     ,
                        id                         = excluded.id                         ,
                        name                       = excluded.name                       ,
                        addresses                  = excluded.addresses                  ,
                        compression                = excluded.compression                ,
                        cert_name                  = excluded.cert_name                  ,
                        max_sequence               = excluded.max_sequence               ,
                        introducer                 = excluded.introducer                 ,
                        index_id                   = excluded.index_id                   ,
                        skip_introduction_removals = excluded.skip_introduction_removals ,
                        encryption_password_token  = excluded.encryption_password_token  
                    ",
                    other_folder.id,
                    device_id,
                    device.name,
                    device_addresses,
                    device.compression,
                    device.cert_name,
                    device.max_sequence,
                    device.introducer,
                    index_id,
                    device.skip_introduction_removals,
                    device.encryption_password_token,
                )
                .execute(&self.db_pool)
                .await
                .expect("Failed to execute query");
            }

            insert_results.push(insert_res);
        }

        sqlx::query!("END TRANSACTION;")
            .execute(&self.db_pool)
            .await
            .unwrap();

        return insert_results;
    }

    fn load_request_id(&self) -> i32 {
        self.request_id
    }

    /// Increments the current value by 1, returning the previous value of [request_id].
    fn fetch_add_request_id(&mut self, val: i32) -> i32 {
        let old_value = self.request_id;
        self.request_id += val;
        old_value
    }

    async fn index(&self, folder: &str, device_id: &DeviceId) -> Option<Index> {
        sqlx::query!("BEGIN TRANSACTION;")
            .execute(&self.db_pool)
            .await
            .unwrap();

        let device_id = device_id.to_string();

        let file_blocks = sqlx::query!(
            r#"
            SELECT f.*, bi.*
            FROM bep_file_info f
                JOIN bep_block_info bi ON f.name = bi.file_name
            WHERE f.folder = ? AND f.device = ?
            ;"#,
            folder,
            device_id,
        )
        .fetch_all(&self.db_pool);

        let file_versions = sqlx::query!(
            r#"
            SELECT f.name,v.id, v.value
            FROM bep_file_info f
                JOIN bep_file_version v ON f.name = v.file_name
            WHERE f.folder = ? and f.device = ?
            ;"#,
            folder,
            device_id,
        )
        .fetch_all(&self.db_pool);

        let mut existing_files = HashMap::<String, FileInfo>::new();

        for file in file_blocks.await.unwrap() {
            if existing_files.contains_key(&file.name) {
                let bi = BlockInfo {
                    offset: file.offset,
                    size: file.bi_size.try_into().unwrap(),
                    hash: file.hash,
                    weak_hash: file.weak_hash.unwrap_or(0).try_into().unwrap(),
                };

                existing_files
                    .get_mut(&file.name)
                    .map(|f| f.blocks.push(bi));
            } else {
                let mut short_id: [u8; 8] = [0; 8];
                for (i, x) in file.modified_by.iter().enumerate() {
                    short_id[i] = *x;
                }
                let mut fi = FileInfo {
                    name: file.name,
                    r#type: file.r#type.try_into().unwrap(),
                    size: file.size,
                    permissions: file.permissions.try_into().unwrap(),
                    modified_s: file.modified_s.try_into().unwrap(),
                    modified_ns: file.modified_ns.try_into().unwrap(),
                    modified_by: u64::from_be_bytes(short_id),
                    deleted: file.deleted == 1,
                    invalid: file.invalid == 1,
                    no_permissions: file.no_permissions == 1,
                    version: Some(syncthing::Vector { counters: vec![] }),
                    sequence: file.sequence.try_into().unwrap(),
                    block_size: file.block_size.try_into().unwrap(),
                    blocks: vec![],
                    symlink_target: file.symlink_target,
                };
                let bi = BlockInfo {
                    offset: file.offset,
                    size: file.bi_size.try_into().unwrap(),
                    hash: file.hash,
                    weak_hash: file.weak_hash.unwrap_or(0).try_into().unwrap(),
                };

                fi.blocks.push(bi);
                existing_files.insert(fi.name.clone(), fi);
            }
        }

        for version in file_versions.await.unwrap() {
            existing_files.get_mut(&version.name).map(|f| {
                let mut short_id: [u8; 8] = [0; 8];
                for (i, x) in version.id.iter().enumerate() {
                    short_id[i] = *x;
                }
                f.version.as_mut().unwrap().counters.push(Counter {
                    id: u64::from_be_bytes(short_id),
                    value: version.value.try_into().unwrap(),
                })
            });
        }

        let index = Index {
            folder: folder.to_string(),
            files: existing_files.into_values().collect(),
        };

        sqlx::query!("END TRANSACTION;")
            .execute(&self.db_pool)
            .await
            .unwrap();

        debug!("The stored index is: {:?}", &index);
        Some(index)
    }

    async fn update_index_block(
        &self,
        request_id: &i32,
        weak_hash: u32,
    ) -> Result<UploadStatus, String> {
        let request = &self.outgoing_requests[request_id];
        debug!("Previously sent request: {:?}", &request);

        sqlx::query!("BEGIN TRANSACTION;")
            .execute(&self.db_pool)
            .await
            .unwrap();

        let device_id = self.config.id.to_string();
        let block_hash = request.hash.clone();
        let insert_res = sqlx::query!(
            r#"
            INSERT INTO bep_block_info (
                file_name,  
                file_folder,  
                file_device,  
                offset,
                bi_size,
                hash,
                weak_hash
            )
            VALUES (?, ?, ?, ?, ?, ?, ?)
            ON CONFLICT(file_name, file_folder, file_device, offset, bi_size, hash) DO NOTHING
            "#,
            request.name,
            request.folder,
            device_id,
            request.offset,
            request.size,
            block_hash,
            weak_hash
        )
        .execute(&self.db_pool)
        .await
        .expect("Failed to execute query");

        let block_count = sqlx::query!(
            r#"
            SELECT fi.block_size AS block_size, fi.size AS byte_size, COUNT(*) as stored_blocks
            FROM bep_file_info AS fi JOIN bep_block_info AS bi ON
                fi.name = bi.file_name
                AND fi.folder = bi.file_folder
                AND fi.device = bi.file_device
            WHERE bi.file_name = ? AND bi.file_folder = ? AND bi.file_device = ?
            GROUP BY 1, 2
            "#,
            request.name,
            request.folder,
            device_id,
        )
        .fetch_one(&self.db_pool)
        .await
        .expect("Failed to execute query");

        sqlx::query!("END TRANSACTION;")
            .execute(&self.db_pool)
            .await
            .unwrap();

        if insert_res.rows_affected() == 0 {
            return Err(format!(
                "The block with hash '{:?}' was already present",
                &request.hash
            ));
        }

        let expected_blocks =
            (block_count.byte_size + block_count.block_size - 1) / block_count.block_size;
        if block_count.stored_blocks == expected_blocks {
            Ok(UploadStatus::AllBlocks)
        } else if block_count.stored_blocks < expected_blocks {
            Ok(UploadStatus::BlocksMissing)
        } else {
            return Err(format!(
                "We ended up storing too many blocks for file: {}",
                &request.name
            ));
        }
    }

    async fn add_missing_files_to_local_index(
        &mut self,
        folder_name: &str,
        missing_files: &Vec<FileInfo>,
    ) -> Result<(), String> {
        let device_id = self.config.id.to_string();

        sqlx::query!("BEGIN TRANSACTION;")
            .execute(&self.db_pool)
            .await
            .unwrap();

        for missing_file in missing_files.into_iter() {
            let modified_by: Vec<u8> = missing_file.modified_by.to_be_bytes().into();
            let insert_res = sqlx::query!(
                r#"
            INSERT INTO bep_file_info (
                folder        ,
                device        ,
                name          ,
                type          ,
                size          ,
                permissions   ,
                modified_s    ,
                modified_ns   ,
                modified_by   ,
                deleted       ,
                invalid       ,
                no_permissions,
                sequence      ,
                block_size    ,
                symlink_target
            )
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            ON CONFLICT(folder, device, name) DO UPDATE SET
                folder         = excluded.folder          ,
                device         = excluded.device          ,
                name           = excluded.name            ,
                type           = excluded.type            ,
                size           = excluded.size            ,
                permissions    = excluded.permissions     ,
                modified_s     = excluded.modified_s      ,
                modified_ns    = excluded.modified_ns     ,
                modified_by    = excluded.modified_by     ,
                deleted        = excluded.deleted         ,
                invalid        = excluded.invalid         ,
                no_permissions = excluded.no_permissions  ,
                sequence       = excluded.sequence        ,
                block_size     = excluded.block_size      ,
                symlink_target = excluded.symlink_target  
            "#,
                folder_name,
                device_id,
                missing_file.name,
                missing_file.r#type,
                missing_file.size,
                missing_file.permissions,
                missing_file.modified_s,
                missing_file.modified_ns,
                modified_by,
                missing_file.deleted,
                missing_file.invalid,
                missing_file.no_permissions,
                missing_file.sequence,
                missing_file.block_size,
                missing_file.symlink_target,
            )
            .execute(&self.db_pool)
            .await
            .expect("Failed to execute query");

            for counter in missing_file
                .version
                .as_ref()
                .expect("There must be a version")
                .counters
                .iter()
            {
                let short_id: Vec<u8> = counter.id.to_be_bytes().into();
                let version_value: i64 = counter.value.try_into().unwrap();
                let insert_res = sqlx::query!(
                    r#"
                    INSERT INTO bep_file_version (
                        file_folder ,
                        file_device ,
                        file_name   ,
                        id          ,
                        value
                    )
                    VALUES (?, ?, ?, ?, ?)
                    ON CONFLICT(file_folder, file_device, file_name, id, value) DO NOTHING
                    "#,
                    folder_name,
                    device_id,
                    missing_file.name,
                    short_id,
                    version_value,
                )
                .execute(&self.db_pool)
                .await
                .expect("Failed to execute query");
            }
        }

        sqlx::query!("END TRANSACTION;")
            .execute(&self.db_pool)
            .await
            .unwrap();

        Ok(())
    }

    // TOOO: better handle conflicting files, also when updating the index
    fn insert_conflicting_files(&mut self, folder: String, conflicting_files: Vec<FileInfo>) {
        for file in conflicting_files.into_iter() {
            self.conflicting_files
                .insert((folder.clone(), file.name.clone()), file);
        }
    }

    async fn file_from_local_index(&self, folder_name: &str, file_name: &str) -> Option<FileInfo> {
        // TODO: test if it is faster to run 3 queries 1 for the file, 1 for the blocks 1 for the
        // versions

        sqlx::query!("BEGIN TRANSACTION;")
            .execute(&self.db_pool)
            .await
            .unwrap();

        let device_id = self.config.id.to_string();

        let file = sqlx::query!(
            r#"
            SELECT f.*
            FROM bep_file_info f
            WHERE f.name = ? AND f.folder = ? AND f.device = ?
            ;"#,
            file_name,
            folder_name,
            device_id,
        )
        .fetch_optional(&self.db_pool);

        let file_blocks = sqlx::query!(
            r#"
            SELECT *
            FROM  bep_block_info
            WHERE file_name = ? AND file_folder = ? AND file_device = ?
            ;"#,
            file_name,
            folder_name,
            device_id,
        )
        .fetch_all(&self.db_pool);

        let file_versions = sqlx::query!(
            r#"
            SELECT *
            FROM  bep_file_version
            WHERE file_name = ? AND file_folder = ? AND file_device = ?
            ;"#,
            file_name,
            folder_name,
            device_id,
        )
        .fetch_all(&self.db_pool);

        let blocks: Vec<BlockInfo> = file_blocks
            .await
            .unwrap()
            .into_iter()
            .map(|bi| BlockInfo {
                offset: bi.offset,
                size: bi.bi_size.try_into().unwrap(),
                hash: bi.hash,
                weak_hash: bi.weak_hash.unwrap_or(0).try_into().unwrap(),
            })
            .collect();

        let versions: Vec<Counter> = file_versions
            .await
            .unwrap()
            .into_iter()
            .map(|v| {
                let mut short_id: [u8; 8] = [0; 8];
                for (i, x) in v.id.iter().enumerate() {
                    short_id[i] = *x;
                }
                Counter {
                    id: u64::from_be_bytes(short_id),
                    value: v.value.try_into().unwrap(),
                }
            })
            .collect();

        let file = file.await.unwrap()?;
        debug!("file {:?}", &file);
        let mut short_id: [u8; 8] = [0; 8];
        for (i, x) in file.modified_by.iter().enumerate() {
            short_id[i] = *x;
        }
        let fi = FileInfo {
            name: file.name,
            r#type: file.r#type.try_into().unwrap(),
            size: file.size,
            permissions: file.permissions.try_into().unwrap(),
            modified_s: file.modified_s.try_into().unwrap(),
            modified_ns: file.modified_ns.try_into().unwrap(),
            modified_by: u64::from_be_bytes(short_id),
            deleted: file.deleted == 1,
            invalid: file.invalid == 1,
            no_permissions: file.no_permissions == 1,
            version: Some(syncthing::Vector { counters: versions }),
            sequence: file.sequence.try_into().unwrap(),
            block_size: file.block_size.try_into().unwrap(),
            blocks: blocks,
            symlink_target: file.symlink_target,
        };

        sqlx::query!("END TRANSACTION;")
            .execute(&self.db_pool)
            .await
            .unwrap();

        Some(fi)
    }

    async fn cluster_config(&self) -> Result<ClusterConfig, String> {
        // TODO: measure if it's better to join or have separate queries
        sqlx::query!("BEGIN TRANSACTION;")
            .execute(&self.db_pool)
            .await
            .unwrap();

        let devices = sqlx::query!(
            r#"
            SELECT *
            FROM bep_devices
            ;"#,
        )
        .fetch_all(&self.db_pool);

        let folders = sqlx::query!(
            r#"
            SELECT *
            FROM bep_folders
            ;"#,
        )
        .fetch_all(&self.db_pool);

        let devices: Vec<(String, syncthing::Device)> = devices
            .await
            .expect("Error occured")
            .into_iter()
            .map(|x| {
                let mut index_id: [u8; 8] = [0; 8];
                trace!("Reading index_id {:?}", &x.index_id);
                for (i, y) in x.index_id.iter().enumerate() {
                    index_id[i] = *y;
                }
                let device_id = DeviceId::try_from(x.id.as_str()).unwrap();
                let device = syncthing::Device {
                    id: device_id.into(),
                    name: x.name,
                    addresses: x.addresses.split(",").map(|y| y.to_string()).collect(),
                    compression: syncthing::Compression::Never.into(), // TODO: update
                    cert_name: x.cert_name,
                    max_sequence: x.max_sequence.try_into().unwrap(),
                    introducer: x.introducer == 1,
                    index_id: u64::from_be_bytes(index_id),
                    skip_introduction_removals: x.skip_introduction_removals == 1,
                    encryption_password_token: x.encryption_password_token,
                };
                (x.folder, device)
            })
            .collect();

        let folders: Vec<(syncthing::Folder)> = folders
            .await
            .expect("Error occured")
            .into_iter()
            .map(|x| {
                let folder = syncthing::Folder {
                    id: x.id.clone(),
                    label: x.label,
                    read_only: x.read_only == 1,
                    ignore_permissions: x.ignore_permissions == 1,
                    ignore_delete: x.ignore_delete == 1,
                    disable_temp_indexes: x.disable_temp_indexes == 1,
                    paused: x.paused == 1,
                    devices: devices
                        .iter()
                        .filter(|(f, d)| f == &x.id)
                        .map(|(f, d)| d.clone())
                        .collect(),
                };
                folder
            })
            .collect();

        sqlx::query!("END TRANSACTION;")
            .execute(&self.db_pool)
            .await
            .unwrap();

        Ok(ClusterConfig { folders })
    }

    fn insert_requests(&mut self, requests: &Vec<Request>) {
        for request in requests.iter() {
            self.outgoing_requests.insert(request.id, request.clone());
        }
    }

    fn get_request<'a>(&'a self, request_id: &i32) -> Option<&'a Request> {
        self.outgoing_requests.get(request_id)
    }

    fn remove_request(&mut self, request_id: &i32) -> Option<Request> {
        self.outgoing_requests.remove(request_id)
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
    pub fn new(config: BepConfig, db_pool: SqlitePool) -> Self {
        let bep_state = BepState::new(config.clone(), db_pool);

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
            {
                self.state
                    .lock()
                    .await
                    .update_cluster_config(&cluster_config)
                    .await;
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

    fn handle_index(&self, received_index: Index) -> Vec<BepReply> {
        debug!("Handling Index");
        trace!("Received Index: {:#?}", &received_index);

        let ems = async move {
            let missing_files = {
                let state = &mut self.state.lock().await;
                state.init_index(&received_index.folder).await;

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

        vec![Box::pin(ems)]
    }

    fn handle_request(&self, request: Request) -> Vec<BepReply> {
        debug!("Handling Request");
        debug!("{:?}", request);

        vec![]
    }

    fn handle_response(&self, response: Response) -> Vec<BepReply> {
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

        vec![Box::pin(res)]
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

        let cluster_config = self
            .state
            .lock()
            .await
            .cluster_config()
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

        // TODO: it should be possible to send more indidces
        let index = self
            .state
            .lock()
            .await
            .index("syncthing-original", &self.config.id)
            .await
            .expect("Index must be present");

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
