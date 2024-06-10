use crate::core::{
    outgoing_requests::OutgoingRequests, GrizolFileInfo, GrizolFolder, UploadStatus,
};
use crate::device_id::DeviceId;
use crate::syncthing::{self, Folder};
use chrono::prelude::*;
use chrono_timesource::TimeSource;
use futures::future::try_join_all;
use sqlx::sqlite::{SqlitePool, SqliteQueryResult};
use std::cmp::Ordering;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
use syncthing::{BlockInfo, ClusterConfig, Counter, FileInfo, Index, Request};
use tokio::sync::Mutex;

/// Stores BlockInfo and addtitional data to simplify processing.
#[derive(Hash, Clone, Debug, Eq, PartialEq)]
pub struct BlockInfoExt {
    pub device_id: DeviceId,
    pub folder: String,
    pub name: String,
    pub hash: Vec<u8>,
    pub size: i32,
    pub offset: i64,
}

#[derive(Copy, Clone, Debug)]
pub enum StorageStatus {
    NotStored = 0,
    StoredLocally = 1,
    _StoredRemotely = 2,
}

impl From<StorageStatus> for i32 {
    fn from(val: StorageStatus) -> Self {
        val as i32
    }
}

#[derive(Debug)]
pub struct BepState<TS: TimeSource<Utc>> {
    clock: Arc<Mutex<TS>>,
    db_pool_read: SqlitePool,
    db_pool_write: SqlitePool,
    sequence: Option<i64>,
    request_id: i32,
    outgoing_requests: OutgoingRequests,
}

impl<TS: TimeSource<Utc>> BepState<TS> {
    pub fn new(db_pool_read: SqlitePool, db_pool_write: SqlitePool, clock: Arc<Mutex<TS>>) -> Self {
        BepState {
            clock,
            db_pool_read,
            db_pool_write,
            sequence: None,
            request_id: 0,
            outgoing_requests: Default::default(),
        }
    }

    async fn next_sequence_id(&mut self) -> i64 {
        if self.sequence.is_none() {
            let max_sequence_record = sqlx::query!(
                r#"
                SELECT MAX(sequence) as max_seq FROM bep_file_info;
                "#,
            )
            .fetch_optional(&self.db_pool_read)
            .await;
            match max_sequence_record {
                Ok(record) => {
                    self.sequence = record.and_then(|r| r.max_seq).map(|x| x.into());
                }
                Err(e) => {
                    warn!(
                        "Error encountered when retrieving max sequence from the database: {:?}",
                        e
                    );
                }
            }
        }

        if let Some(s) = self.sequence.as_mut() {
            *s += 1;
            *s
        } else {
            warn!("No sequence found in the database will start from 1, this might cause issues");
            self.sequence = Some(1);
            1
        }
    }

    pub async fn init_index(&self, folder: &str, device_id: &DeviceId) {
        let device_id = device_id.to_string();
        let mut transaction = self.db_pool_write.begin().await.unwrap();
        let _insert_res = sqlx::query!(
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
        .execute(&mut *transaction)
        .await
        .expect("Failed to execute query");
        transaction.commit().await.unwrap();
    }

    /// Remove index with same id and replace all the info associated.
    pub async fn replace_index(&mut self, index: Index, device_id: &DeviceId) {
        debug!("Replacing index");
        let device_id = device_id.to_string();
        let mut transaction = self.db_pool_write.begin().await.unwrap();

        // We have a cascading removal.
        let _insert_res = sqlx::query!(
            "
                DELETE FROM bep_file_info
                WHERE folder = ? AND device = ?;
            ",
            index.folder,
            device_id,
        )
        .execute(&mut *transaction)
        .await
        .expect("Failed to execute query");

        for file in index.files.into_iter() {
            trace!("Updating index, inserting file {}", &file.name);
            let modified_by: Vec<u8> = file.modified_by.to_be_bytes().into();
            let sequence = self.next_sequence_id().await;
            let _insert_res = sqlx::query!(
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
                index.folder,
                device_id,
                file.name,
                file.r#type,
                file.size,
                file.permissions,
                file.modified_s,
                file.modified_ns,
                modified_by,
                file.deleted,
                file.invalid,
                file.no_permissions,
                sequence,
                file.block_size,
                file.symlink_target,
            )
            .execute(&mut *transaction)
            .await
            .expect("Failed to execute query");

            for counter in file
                .version
                .as_ref()
                .expect("There must be a version")
                .counters
                .iter()
            {
                let short_id: Vec<u8> = counter.id.to_be_bytes().into();
                let version_value: i64 = counter.value.try_into().unwrap();
                let _insert_res = sqlx::query!(
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
                    index.folder,
                    device_id,
                    file.name,
                    short_id,
                    version_value,
                )
                .execute(&mut *transaction)
                .await
                .expect("Failed to execute query");
            }

            // TODO: use enum
            let not_stored: i32 = StorageStatus::NotStored.into();

            for block in file.blocks.iter() {
                let _insert_res = sqlx::query!(
                    r#"
                    INSERT INTO bep_block_info (
                        file_name,  
                        file_folder,  
                        file_device,  
                        offset,
                        bi_size,
                        hash,
                        weak_hash,
                        storage_status
                    )
                    VALUES (?, ?, ?, ?, ?, ?, ?, ?)
                    ON CONFLICT(file_name, file_folder, file_device, offset, bi_size, hash) DO NOTHING
                    "#,
                    file.name,
                    index.folder,
                    device_id,
                    block.offset,
                    block.size,
                    block.hash,
                    block.weak_hash,
                    not_stored,
                )
                .execute(&mut *transaction)
                .await
                .expect("Failed to execute query");
            }
        }
        debug!("about to commit");
        transaction.commit().await.unwrap();
        debug!("committed");
    }

    pub async fn update_cluster_config(
        &mut self,
        other: &ClusterConfig,
        local_device_id: &DeviceId,
    ) -> Vec<SqliteQueryResult> {
        let mut insert_results: Vec<SqliteQueryResult> = vec![];

        let mut transaction = self.db_pool_write.begin().await.unwrap();

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
            .execute(&mut *transaction)
            .await
            .expect("Failed to execute query");

            for device in other_folder.devices.iter() {
                let index_id: Vec<u8> = if device.id == Vec::<u8>::from(local_device_id) {
                    vec![0; 8]
                } else {
                    device.index_id.to_be_bytes().into()
                };
                debug!(
                    "Inserting index id {:?} as {:?}",
                    &device.index_id, &index_id
                );
                let device_addresses = device.addresses.join(",");
                let device_id = DeviceId::try_from(&device.id)
                    .expect("Wrong device id format")
                    .to_string();

                let _insert_res = sqlx::query!(
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
                    0, // when first inserting don't use device.max_sequence as we haven't received any updates from that device yet.
                    device.introducer,
                    index_id,
                    device.skip_introduction_removals,
                    device.encryption_password_token,
                )
                .execute(&mut *transaction)
                .await
                .expect("Failed to execute query");
            }

            insert_results.push(insert_res);
        }

        transaction.commit().await.unwrap();

        debug!("Updated internal tracked folders");

        insert_results
    }

    pub fn load_request_id(&self) -> i32 {
        self.request_id
    }

    /// Increments the current value by [val], returning the previous value of [request_id].
    pub fn fetch_add_request_id(&mut self, val: i32) -> i32 {
        let old_value = self.request_id;
        self.request_id += val;
        old_value
    }

    pub async fn indices(
        &self,
        folder: Option<&String>,
        device_id: Option<&DeviceId>,
        client_device_id: DeviceId,
    ) -> Vec<Index> {
        // TODO: this method is very inefficient as it calls index() multiple times

        // TODO: use a better way to pass the parameters, the API is currently *ugly*
        if folder.is_none() && device_id.is_none() {
            todo!("At least one must be set")
        }

        let device_ids: Vec<DeviceId> = if let Some(id) = device_id {
            vec![*id]
        } else {
            let f = folder.unwrap();
            sqlx::query!(
                r#"
                SELECT device
                FROM bep_index
                WHERE folder = ?
                ;"#,
                f,
            )
            .fetch_all(&self.db_pool_read)
            .await
            .expect("Error occurred")
            .iter()
            .map(|record| DeviceId::try_from(record.device.as_str()).unwrap())
            .collect()
        };

        let folders: Vec<String> = if let Some(f) = folder {
            vec![f.clone()]
        } else {
            let id = device_ids.first().unwrap().to_string();
            let client_device_id = client_device_id.to_string();
            sqlx::query!(
                r#"
                SELECT ind_a.folder
                FROM bep_index ind_a JOIN bep_index ind_b ON ind_a.folder = ind_b.folder
                WHERE ind_a.device = ? and ind_b.device = ?
                ;"#,
                id,
                client_device_id,
            )
            .fetch_all(&self.db_pool_read)
            .await
            .expect("Error occurred")
            .into_iter()
            .map(|record| record.folder)
            .collect()
        };

        let mut res = vec![];
        for folder in folders.iter() {
            for device_id in device_ids.iter() {
                let index = self.index(folder, device_id).await;
                if let Some(i) = index {
                    res.push(i)
                }
            }
        }

        res
    }

    pub async fn index(&self, folder: &str, device_id: &DeviceId) -> Option<Index> {
        let mut transaction = self.db_pool_read.begin().await.unwrap();

        let device_id = device_id.to_string();

        let file_blocks = sqlx::query!(
            r#"
            SELECT f.*, bi.*
            FROM bep_file_info f
                LEFT JOIN bep_block_info bi ON f.name = bi.file_name AND f.folder = bi.file_folder AND f.device = bi.file_device
            WHERE f.folder = ? AND f.device = ?
            ;"#,
            folder,
            device_id,
        )
        .fetch_all(&mut *transaction).await;

        let file_versions = sqlx::query!(
            r#"
            SELECT f.name, v.id, v.value
            FROM bep_file_info f
                JOIN bep_file_version v ON f.name = v.file_name AND f.folder = v.file_folder AND f.device = v.file_device
            WHERE f.folder = ? and f.device = ?
            ;"#,
            folder,
            device_id,
        )
        .fetch_all(&mut *transaction).await;

        transaction.commit().await.unwrap();

        let mut existing_files = HashMap::<String, FileInfo>::new();

        for file in file_blocks.unwrap() {
            if existing_files.contains_key(&file.name) {
                if file.offset.is_some() {
                    let bi = BlockInfo {
                        offset: file.offset.unwrap(),
                        size: file.bi_size.unwrap().try_into().unwrap(),
                        hash: file.hash.unwrap(),
                        weak_hash: file.weak_hash.unwrap_or(0).try_into().unwrap(),
                    };
                    if let Some(f) = existing_files.get_mut(&file.name) {
                        f.blocks.push(bi)
                    }
                }
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
                    modified_s: file.modified_s,
                    modified_ns: file.modified_ns.try_into().unwrap(),
                    modified_by: u64::from_be_bytes(short_id),
                    deleted: file.deleted == 1,
                    invalid: file.invalid == 1,
                    no_permissions: file.no_permissions == 1,
                    version: Some(syncthing::Vector { counters: vec![] }),
                    sequence: file.sequence,
                    block_size: file.block_size.try_into().unwrap(),
                    blocks: vec![],
                    symlink_target: file.symlink_target,
                };
                if file.offset.is_some() {
                    let bi = BlockInfo {
                        offset: file.offset.unwrap(),
                        size: file.bi_size.unwrap().try_into().unwrap(),
                        hash: file.hash.unwrap(),
                        weak_hash: file.weak_hash.unwrap_or(0).try_into().unwrap(),
                    };

                    fi.blocks.push(bi);
                }
                existing_files.insert(fi.name.clone(), fi);
            }
        }

        for version in file_versions.unwrap() {
            if let Some(f) = existing_files.get_mut(&version.name) {
                let mut short_id: [u8; 8] = [0; 8];
                for (i, x) in version.id.iter().enumerate() {
                    short_id[i] = *x;
                }
                f.version.as_mut().unwrap().counters.push(Counter {
                    id: u64::from_be_bytes(short_id),
                    value: version.value.try_into().unwrap(),
                })
            };
        }

        let index = Index {
            folder: folder.to_string(),
            files: existing_files.into_values().collect(),
        };

        trace!("The stored index is: {:?}", &index);
        Some(index)
    }

    pub async fn update_block_storage_state(
        &self,
        request_id: &i32,
        _weak_hash: u32,
        device_id: &DeviceId,
    ) -> Result<UploadStatus, String> {
        let device_id = device_id.to_string();
        let request: &Request = self.outgoing_requests.get(request_id).unwrap();
        debug!("Previously sent request: {:?}", request);

        let mut transaction = self.db_pool_write.begin().await.unwrap();

        let stored_locally: i32 = StorageStatus::StoredLocally.into();
        let block_hash = request.hash.clone();
        let _insert_res = sqlx::query!(
            r#"
            UPDATE OR ROLLBACK bep_block_info
            SET storage_status = ?
            WHERE file_name = ? AND file_folder = ? AND file_device = ? AND hash = ?
            "#,
            stored_locally,
            request.name,
            request.folder,
            device_id,
            block_hash,
        )
        .execute(&mut *transaction)
        .await
        .expect("Failed to execute query");

        let block_count = sqlx::query!(
            r#"
            SELECT fi.block_size AS block_size, fi.size AS byte_size, COUNT(*) as stored_blocks
            FROM bep_file_info AS fi JOIN bep_block_info AS bi ON
                fi.name = bi.file_name
                AND fi.folder = bi.file_folder
                AND fi.device = bi.file_device
            WHERE bi.file_name = ? AND bi.file_folder = ? AND bi.file_device = ? AND bi.storage_status = 1
            GROUP BY 1, 2
            "#,
            request.name,
            request.folder,
            device_id,
        )
        .fetch_one(&mut *transaction)
        .await
        .expect("Failed to execute query");

        // Even if the size of the file is 0 there will still be one block_info
        let expected_blocks = std::cmp::max(
            (block_count.byte_size + block_count.block_size - 1) / block_count.block_size,
            1,
        );

        let file_state = match block_count.stored_blocks.cmp(&expected_blocks) {
            Ordering::Equal => Ok(UploadStatus::AllBlocks),
            Ordering::Less => Ok(UploadStatus::BlocksMissing),
            Ordering::Greater => {
                Err(format!(
                "We ended up storing too many blocks for file: {}, expected blocks {}, actual blocks {}",
                &request.name, expected_blocks, block_count.stored_blocks
                ))
            }
        };

        let storage_backend = "local".to_string();
        if let Ok(UploadStatus::AllBlocks) = file_state {
            let _insert_res = sqlx::query!(
                "
                INSERT INTO bep_file_location (
                    loc_device,
                    loc_folder,
                    loc_name,
                    storage_backend,
                    location
                )
                VALUES (?, ?, ?, ?, ?)
                ON CONFLICT(loc_folder, loc_device, loc_name, storage_backend, location) DO UPDATE SET
                    loc_device       = excluded.loc_device,
                    loc_folder       = excluded.loc_folder,
                    loc_name         = excluded.loc_name,
                    storage_backend  = excluded.storage_backend,
                    location         = excluded.location
                ",
                device_id,
                request.folder,
                request.name,
                storage_backend,
                request.name,
            )
            .execute(&mut *transaction)
            .await
            .expect("Failed to execute query");
        }

        transaction.commit().await.unwrap();

        file_state
    }

    /// Returns a list of [BlockInfo] filtered by the provided parameters
    // TODO: provide a better way to filter rather than passing a parameter
    pub async fn blocks_info(
        &mut self,
        device_id: &DeviceId,
        storage_status: StorageStatus,
    ) -> Vec<BlockInfoExt> {
        let device_id_str = device_id.to_string();
        let storage_status: i32 = storage_status.into();

        let mut transaction = self.db_pool_read.begin().await.unwrap();

        let file_blocks = sqlx::query!(
            r#"
              SELECT *
              FROM  bep_block_info
              WHERE file_device = ?
                AND storage_status = ?
              ;"#,
            device_id_str,
            storage_status
        )
        .fetch_all(&mut *transaction)
        .await;

        transaction.commit().await.unwrap();

        let blocks: Vec<BlockInfoExt> = file_blocks
            .unwrap()
            .into_iter()
            .map(|block| BlockInfoExt {
                device_id: *device_id,
                folder: block.file_folder,
                name: block.file_name,
                hash: block.hash,
                size: block.bi_size.try_into().unwrap(),
                offset: block.offset,
            })
            .collect();

        debug!("Blocks to be requested: {:?}", blocks);

        blocks
    }

    /// Returns top level folders storing all the entries.
    /// [Folder] does not derive Hash, therefore it's not possible to directly use a [HashMap],
    /// however the returned [Vec] guarantees that there are no duplicate [GrizolFolder]s.
    // TODO: what's the best way to filter?? in the SQL? within this fuction, at the client?
    pub async fn top_folders_fuse(&self, device_id: DeviceId) -> Vec<GrizolFolder> {
        let device_id = device_id.to_string();

        let mut transaction = self.db_pool_read.begin().await.unwrap();

        let folders = sqlx::query!(
            r#"
            SELECT DISTINCT(f.rowid) AS f_id, f.*
            FROM bep_folders f JOIN bep_devices d ON f.id = d.folder
            WHERE d.id = ?
            ;"#,
            device_id,
        )
        .fetch_all(&mut *transaction)
        .await;

        transaction.commit().await.unwrap();

        folders
            .unwrap()
            .into_iter()
            .filter(|f| f.f_id.is_some())
            .map(|f| {
                let folder = Folder {
                    id: f.id,
                    label: f.label,
                    read_only: f.read_only == 1,
                    ignore_permissions: f.ignore_permissions == 1,
                    ignore_delete: f.ignore_delete == 1,
                    disable_temp_indexes: f.disable_temp_indexes == 1,
                    paused: f.paused == 1,
                    devices: vec![],
                };
                GrizolFolder {
                    folder,
                    id: f.f_id.unwrap(),
                }
            })
            .collect()
    }

    /// Returns information that can be used for fuse.
    /// [FileInfo] does not derive Hash, therefore it's not possible to directly use a [HashMap],
    /// however the returned [Vec] guarantees that there are no duplicate [GrizolFileInfo]s.
    // TODO: what's the best way to filter?? in the SQL? within this fuction, at the client?
    pub async fn files_fuse(&self, device_id: DeviceId) -> Vec<GrizolFileInfo> {
        let device_id = device_id.to_string();

        let mut transaction = self.db_pool_read.begin().await.unwrap();

        let files = sqlx::query!(
            r#"
            SELECT fin.rowid as r_id, *
            FROM bep_file_location flo RIGHT JOIN bep_file_info fin ON
              flo.loc_folder = fin.folder AND flo.loc_device = fin.device AND flo.loc_name = fin.name
            WHERE fin.device = ?
            ;"#,
            device_id,
        )
        .fetch_all(&mut *transaction).await;

        transaction.commit().await.unwrap();

        // This type is used only within this method
        let mut files_fuse: HashMap<(String, String, String), GrizolFileInfo> = Default::default();

        for file in files.unwrap().into_iter() {
            let mut short_id: [u8; 8] = [0; 8];
            for (i, x) in file.modified_by.iter().enumerate() {
                short_id[i] = *x;
            }
            let fin = FileInfo {
                name: file.name.clone(),
                r#type: file.r#type.try_into().unwrap(),
                size: file.size,
                permissions: file.permissions.try_into().unwrap(),
                modified_s: file.modified_s,
                modified_ns: file.modified_ns.try_into().unwrap(),
                modified_by: u64::from_be_bytes(short_id),
                deleted: file.deleted == 1,
                invalid: file.invalid == 1,
                no_permissions: file.no_permissions == 1,
                version: None,
                sequence: file.sequence,
                block_size: file.block_size.try_into().unwrap(),
                blocks: vec![],
                symlink_target: file.symlink_target,
            };

            let g_fin = GrizolFileInfo {
                file_info: fin,
                id: file.r_id,
                folder: file.folder.clone(),
                // TODO: add file locations
                file_locations: vec![],
            };

            // TODO: add file locations
            files_fuse
                .entry((file.folder, file.device, file.name))
                .or_insert(g_fin);
        }

        files_fuse.into_values().collect()
    }

    pub async fn file_info(
        &self,
        folder_name: &str,
        device_id: DeviceId,
        file_name: &str,
    ) -> Option<FileInfo> {
        // TODO: test if it is faster to run 3 queries 1 for the file, 1 for the blocks 1 for the
        // versions

        let mut transaction = self.db_pool_read.begin().await.unwrap();

        let device_id = device_id.to_string();

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
        .fetch_optional(&mut *transaction)
        .await;

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
        .fetch_all(&mut *transaction)
        .await;

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
        .fetch_all(&mut *transaction)
        .await;

        transaction.commit().await.unwrap();

        let blocks: Vec<BlockInfo> = file_blocks
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

        let file = file.unwrap()?;
        trace!("file {:?}", &file);
        let mut short_id: [u8; 8] = [0; 8];
        for (i, x) in file.modified_by.iter().enumerate() {
            short_id[i] = *x;
        }
        let fi = FileInfo {
            name: file.name,
            r#type: file.r#type.try_into().unwrap(),
            size: file.size,
            permissions: file.permissions.try_into().unwrap(),
            modified_s: file.modified_s,
            modified_ns: file.modified_ns.try_into().unwrap(),
            modified_by: u64::from_be_bytes(short_id),
            deleted: file.deleted == 1,
            invalid: file.invalid == 1,
            no_permissions: file.no_permissions == 1,
            version: Some(syncthing::Vector { counters: versions }),
            sequence: file.sequence,
            block_size: file.block_size.try_into().unwrap(),
            blocks,
            symlink_target: file.symlink_target,
        };

        Some(fi)
    }

    pub async fn cluster_config(
        &self,
        local_device_id: DeviceId,
        client_device_id: DeviceId,
    ) -> Result<ClusterConfig, String> {
        let local_device_id = local_device_id.to_string();
        let client_device_id = client_device_id.to_string();

        // TODO: measure if it's better to join or have separate queries
        let mut transaction = self.db_pool_read.begin().await.unwrap();

        let devices = sqlx::query!(
            r#"
            SELECT *
            FROM bep_devices
            WHERE id = ? OR id = ?
            ;"#,
            local_device_id,
            client_device_id
        )
        .fetch_all(&mut *transaction)
        .await;

        let folders = sqlx::query!(
            r#"
            SELECT fo.*
            FROM bep_folders fo JOIN bep_devices de ON fo.id = de.folder
            WHERE de.id = ? ;"#,
            client_device_id
        )
        .fetch_all(&mut *transaction)
        .await;

        transaction.commit().await.unwrap();

        let devices: Vec<(String, syncthing::Device)> = devices
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
                    addresses: x.addresses.split(',').map(|y| y.to_string()).collect(),
                    compression: syncthing::Compression::Never.into(), // TODO: update
                    cert_name: x.cert_name,
                    max_sequence: x.max_sequence,
                    introducer: x.introducer == 1,
                    index_id: u64::from_be_bytes(index_id),
                    skip_introduction_removals: x.skip_introduction_removals == 1,
                    encryption_password_token: x.encryption_password_token,
                };
                (x.folder, device)
            })
            .collect();

        let folders: Vec<syncthing::Folder> = folders
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
                        .filter(|(f, _d)| f == &x.id)
                        .map(|(_f, d)| d.clone())
                        .collect(),
                };
                folder
            })
            .collect();

        Ok(ClusterConfig { folders })
    }

    // TODO: decide what to do when there are conflicts
    /// Adds file info for a folder and a device
    pub async fn insert_file_info(
        &mut self,
        folder: &str,
        device_id: &DeviceId,
        file_info: &[FileInfo],
    ) {
        let device_id = device_id.to_string();
        let mut transaction = self.db_pool_write.begin().await.unwrap();

        for file in file_info.iter() {
            debug!("Updating index, inserting file {}", &file.name);
            let modified_by: Vec<u8> = file.modified_by.to_be_bytes().into();
            let sequence = self.next_sequence_id().await;
            let _insert_res = sqlx::query!(
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
                folder,
                device_id,
                file.name,
                file.r#type,
                file.size,
                file.permissions,
                file.modified_s,
                file.modified_ns,
                modified_by,
                file.deleted,
                file.invalid,
                file.no_permissions,
                sequence,
                file.block_size,
                file.symlink_target,
            )
            .execute(&mut *transaction)
            .await
            .expect("Failed to execute query");

            for counter in file
                .version
                .as_ref()
                .expect("There must be a version")
                .counters
                .iter()
            {
                let short_id: Vec<u8> = counter.id.to_be_bytes().into();
                let version_value: i64 = counter.value.try_into().unwrap();
                let _insert_res = sqlx::query!(
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
                    folder,
                    device_id,
                    file.name,
                    short_id,
                    version_value,
                )
                .execute(&mut *transaction)
                .await
                .expect("Failed to execute query");
            }

            // TODO: use an enum
            let not_stored: i32 = StorageStatus::NotStored.into();

            for block in file.blocks.iter() {
                let _insert_res = sqlx::query!(
                    r#"
                    INSERT INTO bep_block_info (
                        file_name,  
                        file_folder,  
                        file_device,  
                        offset,
                        bi_size,
                        hash,
                        weak_hash,
                        storage_status
                    )
                    VALUES (?, ?, ?, ?, ?, ?, ?, ?)
                    ON CONFLICT(file_name, file_folder, file_device, offset, bi_size, hash) DO NOTHING
                    "#,
                    file.name,
                    folder,
                    device_id,
                    block.offset,
                    block.size,
                    block.hash,
                    block.weak_hash,
                    not_stored,
                )
                .execute(&mut *transaction)
                .await
                .expect("Failed to execute query");
            }
        }

        transaction.commit().await.unwrap();
    }

    // TODO: This should remove directly the file
    // TODO: add a method _mv_replace_file_info
    pub async fn rm_replace_file_info(
        &mut self,
        folder: &str,
        device_id: &DeviceId,
        file_info: &Vec<FileInfo>,
    ) {
        debug!("About to remove files: {:?}", file_info);
        self.replace_file_info(folder, device_id, file_info, false)
            .await;
    }

    // TODO: use an enum instead of move_file: bool?, it's just an internal method though..
    /// Adds file info for a folder and a device, returns the new destination of the file if the
    /// file is moved.
    async fn replace_file_info(
        &mut self,
        folder: &str,
        device_id: &DeviceId,
        file_info: &[FileInfo],
        move_file: bool,
    ) -> HashMap<String, String> {
        let device_id = device_id.to_string();

        let mut file_dests: HashMap<String, String> = Default::default();
        let mut transaction = self.db_pool_write.begin().await.unwrap();

        for file in file_info.iter() {
            if file.deleted {
                // Load baring debug statement for the integration tests
                debug!(
                    "File '{}' was deleted on device '{}', it will not be deleted here ",
                    &file.name, device_id
                );
                continue;
            }
            // Rename according to
            // https://docs.syncthing.net/users/syncing.html#conflicting-changes
            if move_file {
                debug!("Moving file: {}", file.name);
                let new_path = {
                    let clock = self.clock.lock().await;
                    new_file_path(&file.name, &device_id, clock.now().unwrap())
                };
                // We have a cascading updates.
                let _update_res = sqlx::query!(
                    "
                    PRAGMA foreign_keys = ON;
                    UPDATE OR ROLLBACK bep_file_info
                    SET name = ?
                    WHERE folder = ? AND device = ? AND name = ?; 
                    ",
                    new_path,
                    folder,
                    device_id,
                    file.name,
                )
                .execute(&mut *transaction)
                .await
                .expect("Failed to execute query");

                file_dests.insert(file.name.clone(), new_path);
            } else {
                debug!("Deleting file: {}", file.name);
                // We have a cascading removal.
                let _delete_res = sqlx::query!(
                    "
                    PRAGMA foreign_keys = ON;
                    DELETE FROM bep_file_info
                    WHERE folder = ? AND device = ? AND name = ?; 
                    ",
                    folder,
                    device_id,
                    file.name,
                )
                .execute(&mut *transaction)
                .await
                .expect("Failed to execute query");
            }

            debug!("Updating index, inserting file {:?}", &file);
            let modified_by: Vec<u8> = file.modified_by.to_be_bytes().into();
            let sequence = self.next_sequence_id().await;
            let _insert_res = sqlx::query!(
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
                folder,
                device_id,
                file.name,
                file.r#type,
                file.size,
                file.permissions,
                file.modified_s,
                file.modified_ns,
                modified_by,
                file.deleted,
                file.invalid,
                file.no_permissions,
                sequence,
                file.block_size,
                file.symlink_target,
            )
            .execute(&mut *transaction)
            .await
            .expect("Failed to execute query");

            for counter in file
                .version
                .as_ref()
                .expect("There must be a version")
                .counters
                .iter()
            {
                let short_id: Vec<u8> = counter.id.to_be_bytes().into();
                let version_value: i64 = counter.value.try_into().unwrap();
                let _insert_res = sqlx::query!(
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
                    folder,
                    device_id,
                    file.name,
                    short_id,
                    version_value,
                )
                .execute(&mut *transaction)
                .await
                .expect("Failed to execute query");
            }

            let not_stored: i32 = StorageStatus::NotStored.into();

            for block in file.blocks.iter() {
                let _insert_res = sqlx::query!(
                    r#"
                    INSERT INTO bep_block_info (
                        file_name,
                        file_folder,
                        file_device,
                        offset,
                        bi_size,
                        hash,
                        weak_hash,
                        storage_status
                    )
                    VALUES (?, ?, ?, ?, ?, ?, ?, ?)
                    ON CONFLICT(file_name, file_folder, file_device, offset, bi_size, hash) DO NOTHING
                    "#,
                    file.name,
                    folder,
                    device_id,
                    block.offset,
                    block.size,
                    block.hash,
                    block.weak_hash,
                    not_stored,
                )
                .execute(&mut *transaction)
                .await
                .expect("Failed to execute query");
            }
        }

        transaction.commit().await.unwrap();

        debug!("File destinations: {:?}", file_dests);
        file_dests
    }

    pub async fn rm_file_info(&self, folder: &str, device_id: &DeviceId, file_name: &str) {
        let device_id = device_id.to_string();
        debug!("Deleting file: {}", file_name);

        let mut transaction = self.db_pool_write.begin().await.unwrap();

        // We have a cascading removal.
        let _delete_res = sqlx::query!(
            "
                PRAGMA foreign_keys = ON;
                DELETE FROM bep_file_info
                WHERE folder = ? AND device = ? AND name = ?; 
            ",
            folder,
            device_id,
            file_name,
        )
        .execute(&mut *transaction)
        .await
        .expect("Failed to execute query");

        transaction.commit().await.unwrap();
    }

    pub async fn update_file_locations(
        &self,
        folder: &str,
        device_id: &DeviceId,
        file_name: &str,
        locations: Vec<(String, String)>,
    ) {
        let device_id = device_id.to_string();
        let file_name = file_name.to_string();
        let mut transaction = self.db_pool_write.begin().await.unwrap();

        for location in locations.into_iter() {
            let _insert_res = sqlx::query!(
                "
            INSERT INTO bep_file_location (
                loc_folder      ,
                loc_device      ,
                loc_name        ,
                storage_backend ,
                location        
            )
            VALUES (?, ?, ?, ?, ?)
            ON CONFLICT(loc_folder, loc_device, loc_name, storage_backend, location) DO UPDATE SET
                loc_device      = excluded.loc_device     ,
                loc_folder      = excluded.loc_folder     ,
                loc_name        = excluded.loc_name       ,
                storage_backend = excluded.storage_backend,
                location        = excluded.location
            ",
                folder,
                device_id,
                file_name,
                location.0,
                location.1
            )
            .execute(&mut *transaction)
            .await
            .expect("Failed to execute query");
        }
        transaction.commit().await.unwrap();
    }

    pub async fn remove_file_locations(
        &self,
        folder: &str,
        device_id: &DeviceId,
        file_name: &str,
        storage_backends: Vec<String>,
    ) {
        let device_id = device_id.to_string();
        let file_name = file_name.to_string();
        let mut transaction = self.db_pool_write.begin().await.unwrap();

        for storage_backend in storage_backends.into_iter() {
            let _insert_res = sqlx::query!(
                "
                DELETE FROM bep_file_location 
                WHERE TRUE
                    AND loc_folder       = ?
                    AND loc_device       = ?
                    AND loc_name         = ?
                    AND storage_backend  = ?
                ",
                folder,
                device_id,
                file_name,
                storage_backend,
            )
            .execute(&mut *transaction)
            .await
            .expect("Failed to execute query");
        }

        transaction.commit().await.unwrap();
    }

    /// Returns the inserted requests
    pub fn insert_requests(&mut self, requests: Vec<Request>) -> Vec<Request> {
        self.outgoing_requests.remove_old_requests();

        requests
            .into_iter()
            .filter_map(|request| self.outgoing_requests.insert(request.id, request))
            .collect()
    }

    /// Ensures that pending requests don't increase the limits set by [max_pending] and
    /// [max_storage_area]: There can be at most [max_pending] requesting at most
    /// [max_storage_area] data.  Returns the new requests that have been added if any.
    pub fn update_pending_requests(
        &mut self,
        max_pending: usize,
        max_storage_area: usize,
    ) -> Vec<Request> {
        self.outgoing_requests
            .update_pending_requests(max_pending, max_storage_area)
    }

    pub fn get_request<'a>(&'a self, request_id: &i32) -> Option<&'a Request> {
        self.outgoing_requests.get(request_id)
    }

    pub fn remove_request(&mut self, request_id: &i32) -> Option<Request> {
        self.outgoing_requests.remove(request_id)
    }

    pub async fn is_file_in_read_cache(
        &self,
        device_id: &DeviceId,
        folder: &str,
        file: &str,
    ) -> bool {
        let device_id = device_id.to_string();
        let file_count = sqlx::query!(
            "
                SELECT COUNT(cache_file_name) as c
                FROM bep_local_cache
                WHERE TRUE
                  AND cache_folder = ?
                  AND cache_device = ?
                  AND cache_file_name = ?
                ;
            ",
            folder,
            device_id,
            file,
        )
        .fetch_optional(&self.db_pool_read)
        .await
        .expect("Failed to execute query")
        .map(|fc| fc.c);

        match file_count {
            Some(1) => true,
            Some(0) => false,
            Some(c) => {
                error!(
                    "This should not happen. There are '{}' instances of '{}'. There is likely an error in the database schema.",
                    c, file
                );
                false
            }
            None => false,
        }
    }

    // TODO: Improve cache eviction algorithm
    /// Frees the cache using a FIFO mechanism.
    pub async fn free_read_cache(
        &self,
        cache_size: u64,
        needed_bytes: u64,
    ) -> Result<Vec<(String, String)>, String> {
        let mut transaction = self.db_pool_write.begin().await.unwrap();

        // TODO: deal with symlinks
        let files = sqlx::query!(
            r#"
            SELECT lc.cache_folder, lc.cache_file_name, lc.cache_device, lc.timestamp_added, fi.size
            FROM bep_local_cache lc JOIN bep_file_info fi ON TRUE
              AND lc.cache_folder = fi.folder
              AND lc.cache_file_name = fi.name
              AND lc.cache_device = fi.device
            WHERE fi.type = 0
            ORDER BY lc.timestamp_added ASC
        ;"#
        )
        .fetch_all(&mut *transaction)
        .await
        .expect("Error occurred");

        let used_bytes: u64 = files.iter().map(|f| u64::try_from(f.size).unwrap()).sum();
        let mut available_bytes = cache_size - used_bytes;
        let mut removed_files = vec![];
        for file in files.iter() {
            if available_bytes >= needed_bytes {
                break;
            }
            available_bytes += u64::try_from(file.size).unwrap();
            removed_files.push(file);
        }

        // TODO: do this in a SQL single statement if possible
        for removed_file in removed_files.iter() {
            // We have cascading removal.
            sqlx::query!(
                "
                DELETE FROM bep_local_cache WHERE TRUE
                    AND cache_device = ?
                    AND cache_folder = ?
                    AND cache_file_name = ?
                ",
                removed_file.cache_device,
                removed_file.cache_folder,
                removed_file.cache_file_name,
            )
            .execute(&mut *transaction)
            .await
            .unwrap();
        }

        transaction.commit().await.unwrap();

        let res = removed_files
            .iter()
            .map(|&f| (f.cache_folder.clone(), f.cache_file_name.clone()))
            .collect();
        Ok(res)
    }

    pub async fn update_read_cache(&self, device_id: &DeviceId, folder: &str, file: &str) {
        let device_id = device_id.to_string();

        let now = {
            let clock = self.clock.lock().await;
            clock
                .now()
                .map(|date_time| date_time.timestamp_millis())
                .unwrap_or(0)
        };

        let mut transaction = self.db_pool_write.begin().await.unwrap();
        let _insert_res = sqlx::query!(
            "
                INSERT INTO bep_local_cache (
                    cache_folder    ,
                    cache_device    ,
                    cache_file_name ,
                    timestamp_added 
                )
                VALUES (?, ?, ?, ?)
                ON CONFLICT(cache_folder, cache_device, cache_file_name) DO UPDATE SET
                    cache_folder    = excluded.cache_folder    ,
                    cache_device    = excluded.cache_device    ,
                    cache_file_name = excluded.cache_file_name 
                ",
            folder,
            device_id,
            file,
            now
        )
        .execute(&mut *transaction)
        .await
        .expect("Failed to execute query");

        transaction.commit().await.unwrap();
    }
}

/// Rename a file according to
/// https://docs.syncthing.net/users/syncing.html#conflicting-changes
fn new_file_path(old_file_path: &str, device_id: &str, now: DateTime<Utc>) -> String {
    let old_path = PathBuf::from(old_file_path);

    let extension = old_path
        .extension()
        .map(PathBuf::from)
        .map(|x| x.to_str().unwrap().to_string())
        .map(|x| format!(".{}", x))
        .unwrap_or("".to_string());

    let mut name = old_path
        .clone()
        .file_name()
        .map(PathBuf::from)
        .expect("Invalid file name")
        .file_stem()
        .unwrap()
        .to_os_string();
    let date = now.format("%Y%m%d");
    let time = now.format("%H%M%S");
    let modified_by = &device_id;
    name.push(format!(
        ".sync-conflict-{}-{}-{}{}",
        date, time, modified_by, extension
    ));
    let new_name: PathBuf = name.into();

    let new_path_parts = [old_path.parent().map(|x| x.into()), Some(new_name.clone())];

    let new_path: PathBuf = new_path_parts.iter().flatten().collect();
    new_path
        .to_str()
        .expect("Not possible to convert into String")
        .into()
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn new_file_path_wo_parent_adds_suffix() {
        let now = Utc.with_ymd_and_hms(1970, 1, 2, 3, 4, 5).unwrap();

        let new_file_path = new_file_path("ABC", "deviceid", now);

        assert_eq!(
            new_file_path,
            "ABC.sync-conflict-19700102-030405-deviceid".to_string()
        );
    }

    #[test]
    fn new_file_path_w_parent_adds_suffix() {
        let now = Utc.with_ymd_and_hms(1970, 1, 2, 3, 4, 5).unwrap();

        let new_file_path = new_file_path("ABC/ABC", "deviceid", now);

        assert_eq!(
            new_file_path,
            "ABC/ABC.sync-conflict-19700102-030405-deviceid".to_string()
        );
    }

    #[test]
    fn new_file_path_w_ext_adds_suffix() {
        let now = Utc.with_ymd_and_hms(1970, 1, 2, 3, 4, 5).unwrap();

        let new_file_path = new_file_path("ABC/ABC.xyz", "deviceid", now);

        assert_eq!(
            new_file_path,
            "ABC/ABC.sync-conflict-19700102-030405-deviceid.xyz".to_string()
        );
    }

    #[test]
    fn new_file_path_w_ext_wo_parent_adds_suffix() {
        let now = Utc.with_ymd_and_hms(1970, 1, 2, 3, 4, 5).unwrap();

        let new_file_path = new_file_path("ABC.xyz", "deviceid", now);

        assert_eq!(
            new_file_path,
            "ABC.sync-conflict-19700102-030405-deviceid.xyz".to_string()
        );
    }
}
