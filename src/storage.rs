use crate::syncthing::{ClusterConfig, Index, Request};
use crate::DeviceId;
use prost::Message;
use sha2::{Digest, Sha256};
use std::io;
use tokio::fs::{File, OpenOptions};
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt, SeekFrom};

// TODO: verify this is a reasonable size
const BUF_SIZE: usize = 1 << 14; // 16 KiB

pub struct StorageManager {
    cluster_config_path: String,
    stage_dir: String,
}

impl StorageManager {
    pub fn new(cluster_config_path: String, stage_dir: String) -> Self {
        StorageManager {
            cluster_config_path,
            stage_dir,
        }
    }
    // TODO: this is just a temporary hack, remove this
    pub async fn save_cluster_config(&self, cluster_config: &ClusterConfig) -> io::Result<()> {
        let bytes = cluster_config.encode_to_vec();

        let mut file = File::create(&self.cluster_config_path).await?;
        file.write_all(&bytes).await?;
        file.flush().await?;

        Ok(())
    }

    // TODO: this is just a temporary hack, remove this
    pub async fn restore_cluster_config(&self) -> io::Result<ClusterConfig> {
        let mut file = File::open(&self.cluster_config_path).await?;
        let mut buf: [u8; 1 << 16] = [0; 1 << 16];
        let read_bytes = file.read(&mut buf[..]).await?;

        let cluster_config = ClusterConfig::decode(&buf[..read_bytes])?;

        Ok(cluster_config)
    }

    pub async fn save_client_index(&self, index: &Index) -> io::Result<()> {
        todo!();
    }

    pub async fn store_block(
        &self,
        data: Vec<u8>,
        request: &Request,
        file_size: u64,
    ) -> io::Result<()> {
        debug!("Start storing block");
        // TODO: the file should already be there
        let mut file = self.create_empty_file(&request.name, file_size).await?;

        let offset = request.offset.try_into().unwrap();

        trace!("offset: {}", &offset);
        trace!("block {:?}", &data);

        file.seek(SeekFrom::Start(offset)).await?;

        file.write_all(&data).await?;
        let res = file.flush().await;

        // Compute hash of the written block to double check correctness.
        file.seek(SeekFrom::Start(offset)).await?;
        let mut buf: Vec<u8> = Vec::with_capacity(data.len());
        buf.resize(data.len(), 0);
        file.read_exact(&mut buf).await?;
        let hasher_sha256 = Sha256::new_with_prefix(&buf);
        let hash = hasher_sha256.finalize().to_vec();
        if hash != request.hash {
            error!(
                "Hash of written data is different from the one requested in request '{}'.",
                request.id
            );
            debug!("Data received: {:?}", &data);
            debug!("Data written:  {:?}", &buf);
            debug!("Request hash: {:?}", &request.hash);
            debug!("Data hash:    {:?}", &hash);
        }

        debug!("Finished storing block");
        res
    }

    // Always creates a new file if it does not exist
    async fn create_empty_file(&self, rel_path: &str, file_size: u64) -> io::Result<(File)> {
        debug!(
            "Creating new file of size {} under {}",
            &file_size, rel_path
        );
        // TODO: create the folder first if that does not exist
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(format!("{}/{}", self.stage_dir, rel_path))
            .await?;

        file.set_len(file_size).await?;

        Ok(file)
    }
}

// pub fn data_from_file_block(
//     dir_path: &str,
//     file_info: &syncthing::FileInfo,
//     block_info: &syncthing::BlockInfo,
// ) -> Result<Vec<u8>, String> {
//     let mut file =
//         File::open(format!("{}/{}", dir_path, &file_info.name)).map_err(|e| e.to_string())?;
//     file.seek(io::SeekFrom::Start(block_info.offset as u64))
//         .map_err(|e| e.to_string())?;

//     let mut data: Vec<u8> = vec![0; block_info.size as usize];

//     file.read_exact(&mut data[..]).map_err(|e| e.to_string())?;

//     Ok(data)
// }

// fn file_type(metadata: &fs::Metadata) -> Result<syncthing::FileInfoType, io::Error> {
//     if metadata.is_dir() {
//         return Ok(syncthing::FileInfoType::Directory);
//     }

//     if metadata.is_file() {
//         return Ok(syncthing::FileInfoType::File);
//     }

//     if metadata.is_symlink() {
//         return Ok(syncthing::FileInfoType::Symlink);
//     }

//     Err(io::Error::new(
//         io::ErrorKind::Other,
//         format!("File type not supported"),
//     ))
// }

// fn modified(metadata: &fs::Metadata) -> Result<(i64, i32), io::Error> {
//     let duration_since_unix = metadata
//         .modified()?
//         .duration_since(SystemTime::UNIX_EPOCH)
//         .map_err(|e| {
//             io::Error::new(
//                 io::ErrorKind::Other,
//                 format!("Cannot establish last modification time: {}", e.to_string()),
//             )
//         })?;
//     let ns = duration_since_unix.as_nanos();
//     let s = (ns / 1_000_000_000) as i64;
//     let ns_part = (ns % 1_000_000_000) as i32;
//     Ok((s, ns_part))
// }

// struct Database {
//     ops_clock: AtomicI64,
// }

// impl Database {
//     // TODO: find a better name
//     fn perform_operation(&mut self) -> i64 {
//         self.ops_clock.fetch_add(1, Ordering::SeqCst)
//     }
// }

// pub fn index_from_path(
//     folder_name: &str,
//     dir_path: &Path,
//     device_id: &DeviceId,
// ) -> Result<syncthing::Index, io::Error> {
//     if !fs::metadata(dir_path)?.is_dir() {
//         todo!();
//     }

//     let mut db = Database {
//         ops_clock: 0.into(),
//     };

//     let mut files: Vec<syncthing::FileInfo> = vec![];

//     for entry in fs::read_dir(dir_path)? {
//         let entry = entry?;
//         let path = entry.path();
//         if path.is_file() {
//             let file_info = file_info_from_path(&path, &device_id, &mut db)?;
//             files.push(file_info);
//         } else {
//             todo!();
//         }
//     }

//     Ok(syncthing::Index {
//         folder: folder_name.to_string(),
//         files,
//     })
// }

// fn file_info_from_path(
//     file_path: &Path,
//     device_id: &DeviceId,
//     db: &mut Database,
// ) -> Result<syncthing::FileInfo, io::Error> {
//     let metadata = fs::metadata(file_path)?;

//     // FIXME: this is wrong we want the relative position of the file to the folder
//     let file_name = file_path
//         .file_name()
//         .ok_or(io::Error::new(io::ErrorKind::Other, "Path not supported"))?
//         .to_str()
//         .ok_or(io::Error::new(
//             io::ErrorKind::Other,
//             "Path is not valid UTF",
//         ))?
//         .to_string();
//     let (modified_s, modified_ns) = modified(&metadata)?;

//     let version = Some(syncthing::Vector {
//         counters: vec![syncthing::Counter {
//             id: device_id.into(),
//             value: 0,
//         }],
//     });

//     let base = syncthing::FileInfo {
//         name: file_name,
//         permissions: metadata.permissions().mode(),
//         modified_s,
//         modified_ns,
//         deleted: false,
//         invalid: false,
//         no_permissions: false,
//         version,
//         ..Default::default()
//     };

//     match file_type(&metadata)? {
//         syncthing::FileInfoType::File => {
//             // TODO: use 1<<24 again when we are done with testing.
//             // let block_size = 1 << 24; // 16 MiB
//             let block_size = 1 << 18; // 16 MiB
//             Ok(syncthing::FileInfo {
//                 r#type: syncthing::FileInfoType::File.into(),
//                 size: metadata.len() as i64,
//                 sequence: db.perform_operation(),
//                 block_size,
//                 blocks: blocks_from_path(file_path, block_size as usize)?,
//                 ..base
//             })
//         }
//         syncthing::FileInfoType::Directory => todo!(),
//         syncthing::FileInfoType::Symlink => todo!(),
//         _ => Err(io::Error::new(
//             io::ErrorKind::Other,
//             "File type not supported",
//         )),
//     }
// }

// fn blocks_from_path(
//     file_path: &Path,
//     block_size: usize,
// ) -> Result<Vec<syncthing::BlockInfo>, io::Error> {
//     let file = File::open(file_path)?;
//     let mut file_reader = BufReader::new(file);
//     let mut buf: [u8; BUF_SIZE] = [0; BUF_SIZE];
//     let mut res: Vec<syncthing::BlockInfo> = Default::default();

//     if BUF_SIZE >= block_size {
//         let mut offset: i64 = 0;
//         loop {
//             let read_data_len = file_reader.read(&mut buf)?;
//             if read_data_len == 0 {
//                 break;
//             }
//             for i in 0..((read_data_len + block_size - 1) / block_size) {
//                 let start = i * block_size;
//                 let end = std::cmp::min((i + 1) * block_size, read_data_len);
//                 // TODO: check performance if we have a single hasher for all blocks instead of
//                 // creating a new one for each block.
//                 let hasher_sha256 = Sha256::new_with_prefix(&buf[start..end]);
//                 let block = syncthing::BlockInfo {
//                     offset,
//                     size: (end - start) as i32,
//                     hash: hasher_sha256.finalize().to_vec(),
//                     weak_hash: 0, // FIXME
//                 };
//                 offset += block_size as i64;
//                 res.push(block);
//             }
//         }
//     } else {
//         let mut pushed_to_hasher: usize = 0;
//         let mut offset: i64 = 0;
//         let mut hasher_sha256 = Sha256::new();

//         loop {
//             let read_data_len = file_reader.read(&mut buf)?;
//             if read_data_len == 0 {
//                 break;
//             }
//             if pushed_to_hasher + read_data_len < block_size {
//                 hasher_sha256.update(&buf[..read_data_len]);
//                 pushed_to_hasher += read_data_len;
//             } else {
//                 let to_add_to_hasher = block_size - pushed_to_hasher;
//                 hasher_sha256.update(&buf[..to_add_to_hasher]);
//                 pushed_to_hasher += to_add_to_hasher;

//                 let block = syncthing::BlockInfo {
//                     offset: offset,
//                     size: pushed_to_hasher as i32,
//                     hash: hasher_sha256.finalize_reset().to_vec(),
//                     weak_hash: 0, // FIXME
//                 };
//                 offset += block_size as i64;
//                 res.push(block);

//                 hasher_sha256.update(&buf[to_add_to_hasher..]);
//                 pushed_to_hasher = read_data_len - to_add_to_hasher;
//             }
//         }

//         if pushed_to_hasher > 0 {
//             let block = syncthing::BlockInfo {
//                 offset: offset,
//                 size: pushed_to_hasher as i32,
//                 hash: hasher_sha256.finalize().to_vec(),
//                 weak_hash: 0, // FIXME
//             };
//             offset += 1;
//             res.push(block);
//         }
//     }

//     Ok(res)
// }

#[cfg(test)]
mod tests {
    use crate::storage::StorageManager;
    use crate::syncthing::{ClusterConfig, Index, Request};
    use std::path::Path;

    // TODO: use a better file path
    fn file_path() -> String {
        "/home/marco/test_000/test_1".to_string()
    }

    #[tokio::test]
    async fn store_block__new_file__succeeds() {
        let storage_manager = StorageManager::new(
            format!("/tmp/grizol_cluster_config"),
            format!("/tmp/grizol_staging"),
        );

        let data = vec![0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF];
        let request = Request {
            id: 0,
            folder: format!("test_dir"),
            name: format!("a_file"),
            offset: 0,
            size: data.len() as i32,
            hash: vec![],
            from_temporary: false,
        };

        let result = storage_manager.store_block(data, &request).await;

        match result {
            Ok(x) => assert_eq!(0, 0),
            Err(e) => {
                assert_eq!("No error expected", format!("{}", e))
            }
        }
    }

    // #[tokio::test]
    // async fn create_emtpy_file__new_location__succeeds() {
    //     let storage_manager = StorageManager::new(
    //         format!("/tmp/grizol_cluster_config"),
    //         format!("/tmp/grizol_staging"),
    //     );

    //     let result = storage_manager.create_empty_file("bubu", 123456).await;

    //     match result {
    //         Ok(x) => assert_eq!(0, 0),
    //         Err(e) => {
    //             assert_eq!("No error expected", format!("{}", e))
    //         }
    //     }
    // }

    // #[test]
    // fn hash_file_blocks_buf_size_lt_block_size() {
    //     let block_size = 1 << 16; // 128 KiB

    //     let blocks: Vec<syncthing::BlockInfo> =
    //         blocks_from_path(Path::new(&file_path()), block_size).unwrap();

    //     let blocks_control = vec![syncthing::BlockInfo {
    //         offset: 0,
    //         size: 10,
    //         // This is the sha256sum of
    //         hash: vec![
    //             0x9a, 0x89, 0xc6, 0x8c, 0x4c, 0x5e, 0x28, 0xb8, 0xc4, 0xa5, 0x56, 0x76, 0x73, 0xd4,
    //             0x62, 0xff, 0xf5, 0x15, 0xdb, 0x46, 0x11, 0x6f, 0x99, 0x00, 0x62, 0x4d, 0x09, 0xc4,
    //             0x74, 0xf5, 0x93, 0xfb,
    //         ],
    //         weak_hash: 0,
    //     }];
    //     assert!(blocks == blocks_control);
    // }
}
