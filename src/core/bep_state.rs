use crate::core::{BepConfig, UploadStatus};
use crate::device_id::DeviceId;
use crate::syncthing;
use sqlx::sqlite::{SqlitePool, SqliteQueryResult};
use std::collections::{BTreeMap, HashMap, HashSet};
use syncthing::{
    BlockInfo, Close, ClusterConfig, Compression, Counter, ErrorCode, FileInfo, Header, Hello,
    Index, IndexUpdate, MessageType, Ping, Request, Response,
};

#[derive(Debug)]
pub struct BepState {
    config: BepConfig,
    db_pool: SqlitePool,
    indices: HashMap<String, HashMap<DeviceId, syncthing::Index>>,
    cluster: ClusterConfig,
    sequence: u64,
    request_id: i32,
    conflicting_files: HashMap<(String, String), FileInfo>,
    outgoing_requests: HashMap<i32, Request>,
}

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

    pub async fn init_index(&self, folder: &str) {
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

    pub async fn update_cluster_config(&mut self, other: &ClusterConfig) -> Vec<SqliteQueryResult> {
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

        debug!("Updated internal tracked folders");

        return insert_results;
    }

    pub fn load_request_id(&self) -> i32 {
        self.request_id
    }

    /// Increments the current value by 1, returning the previous value of [request_id].
    pub fn fetch_add_request_id(&mut self, val: i32) -> i32 {
        let old_value = self.request_id;
        self.request_id += val;
        old_value
    }

    pub async fn indices(&self, device_id: &DeviceId) -> Vec<Index> {
        // TODO: this is very inefficient: improve

        let id = device_id.to_string();
        let folders = sqlx::query!(
            r#"
            SELECT folder
            FROM bep_index
            WHERE device = ?
            ;"#,
            id,
        )
        .fetch_all(&self.db_pool)
        .await
        .expect("Error occurred");

        let mut res = vec![];
        for record in folders.iter() {
            let index = self.index(&record.folder, device_id).await;
            if let Some(i) = index {
                res.push(i)
            }
        }

        res
    }

    pub async fn index(&self, folder: &str, device_id: &DeviceId) -> Option<Index> {
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

        trace!("The stored index is: {:?}", &index);
        Some(index)
    }

    pub async fn update_index_block(
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

    pub async fn add_missing_files_to_local_index(
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
    pub fn insert_conflicting_files(&mut self, folder: String, conflicting_files: Vec<FileInfo>) {
        for file in conflicting_files.into_iter() {
            self.conflicting_files
                .insert((folder.clone(), file.name.clone()), file);
        }
    }

    pub async fn file_from_local_index(
        &self,
        folder_name: &str,
        file_name: &str,
    ) -> Option<FileInfo> {
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

    pub async fn cluster_config(&self) -> Result<ClusterConfig, String> {
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

    pub fn insert_requests(&mut self, requests: &Vec<Request>) {
        for request in requests.iter() {
            self.outgoing_requests.insert(request.id, request.clone());
        }
    }

    pub fn get_request<'a>(&'a self, request_id: &i32) -> Option<&'a Request> {
        self.outgoing_requests.get(request_id)
    }

    pub fn remove_request(&mut self, request_id: &i32) -> Option<Request> {
        self.outgoing_requests.remove(request_id)
    }
}
