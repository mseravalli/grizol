use crate::syncthing::Request;

use crate::file_writer::{self, FileWriter};
use crate::GrizolConfig;
use dashmap::DashMap;
use futures::future::try_join_all;
use regex::Regex;
use sha2::{Digest, Sha256};
use std::cell::RefCell;
use std::collections::HashMap;
use std::io;
use std::path::Path;
use std::process::ExitStatus;
use std::rc::Rc;
use tokio::fs::{remove_file, File, OpenOptions};
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt, SeekFrom};
use tokio::process::Command;
use tokio::sync::Mutex;

// This should *not* be async, we want to wait for this to finish and then process the result.
fn storage_backends_from_conf(rclone_config: &Option<String>) -> Vec<String> {
    let config_location = rclone_config
        .as_ref()
        .map(|x| format!("--config={}", x))
        .unwrap_or("".to_string());
    let command = &["rclone", &config_location, "config", "show"];
    info!("Running: `{}`", command.join(" "));
    let output = std::process::Command::new(command[0])
        .args(&command[1..])
        .output()
        .unwrap();
    let data = std::str::from_utf8(&output.stdout).unwrap();

    // TODO: verify if this is makes always sense
    let re = Regex::new(r"^\[(.*)\]$").unwrap();
    let res = data
        .lines()
        .map(|x| x.trim())
        .filter(|x| re.is_match(x))
        .map(|x| re.captures(x).unwrap().get(1).unwrap().as_str().to_string())
        .collect();
    info!("Storage backends: {:?}", res);
    res
}

pub struct StorageManager {
    config: GrizolConfig,
    storage_backends: Vec<String>,
    // TODO: make this thread safe
    file_writers: DashMap<String, FileWriter>,
}

impl StorageManager {
    pub fn new(config: GrizolConfig) -> Self {
        let storage_backends = storage_backends_from_conf(&config.rclone_config);
        StorageManager {
            config,
            storage_backends,
            file_writers: Default::default(),
        }
    }

    pub async fn store_block_concurrently(
        &self,
        data: Vec<u8>,
        request: &Request,
        file_size: u64,
        chunk_size: u64,
    ) -> io::Result<()> {
        let file_rel_path = format!("{}/{}", &request.folder, &request.name);
        let abs_path = format!("{}/{}", self.config.local_base_dir, file_rel_path);
        let parent = Path::new(&abs_path).parent().unwrap();
        debug!("parent: {:?}", parent);
        tokio::fs::create_dir_all(parent).await?;

        let file_writer =
            self.file_writers
                .entry(file_rel_path.clone())
                .or_insert(FileWriter::new(
                    abs_path,
                    file_size as usize,
                    chunk_size as usize,
                ));

        let offset = request.offset.try_into().unwrap();
        file_writer.write_chunk(&data, offset).await
    }

    pub async fn conclude_block_storage(
        &self,
        request: &Request,
        file_size: u64,
        chunk_size: u64,
    ) -> io::Result<()> {
        let file_rel_path = format!("{}/{}", &request.folder, &request.name);
        // In theory it could happen that all the blocks are written, but the program crashes
        // before the consolidation, therefore we recreate the writer. If the blocks are not there
        // the consolidate step will fail.
        let file_writer =
            self.file_writers
                .entry(file_rel_path.clone())
                .or_insert(FileWriter::new(
                    file_rel_path,
                    file_size as usize,
                    chunk_size as usize,
                ));
        file_writer.consolidate().await
    }

    pub async fn store_block(
        &self,
        data: Vec<u8>,
        request: &Request,
        file_size: u64,
    ) -> io::Result<()> {
        debug!("Start storing block");
        // TODO: the file should already be there
        let file_rel_path = format!("{}/{}", &request.folder, &request.name);
        let mut file = self.create_empty_file(&file_rel_path, file_size).await?;

        let offset = request.offset.try_into().unwrap();

        file.seek(SeekFrom::Start(offset)).await?;

        file.write_all(&data).await?;
        let res = file.flush().await;

        // Compute hash of the written block to double check correctness.
        file.seek(SeekFrom::Start(offset)).await?;
        let mut buf: Vec<u8> = vec![0; data.len()];
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
    async fn create_empty_file(&self, rel_path: &str, file_size: u64) -> io::Result<File> {
        let abs_path = format!("{}/{}", self.config.local_base_dir, rel_path);
        let parent = Path::new(&abs_path).parent().unwrap();
        debug!("parent: {:?}", parent);
        tokio::fs::create_dir_all(parent).await?;
        debug!(
            "Creating new file of size {} under {}",
            &file_size, rel_path
        );
        // TODO: create the folder first if that does not exist
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(abs_path)
            .await?;

        file.set_len(file_size).await?;

        Ok(file)
    }

    // TODO: Consider using a struct instread of (String, String)
    pub async fn cp_remote_to_read_cache(&self, folder: &str, name: &str) -> Result<(), String> {
        // FIXME: add a way to select preferred backend
        let storage_backend = "gcs";

        let bucket = self.config.remote_base_dir.clone();
        let orig = format!("{storage_backend}:{bucket}/{folder}/{name}");
        let dest = format!("{}/{}/{}", self.config.read_cache_dir, folder, name);
        let rclone_command = &["copyto", orig.as_str(), dest.as_str()];
        self.run_rclone(rclone_command)
            .await
            .map(|_| ())
            .map_err(|e| format!("Failed to run command: {:?}", e))
    }

    // TODO: Consider using a struct instread of (String, String)
    pub async fn cp_local_to_remote(
        &self,
        folder: &str,
        name: &str,
    ) -> Result<Vec<(String, String)>, String> {
        let orig = format!("{}/{}/{}", self.config.local_base_dir, folder, name);

        let mut res = vec![];
        for storage_backend in self.storage_backends.iter() {
            let bucket = self.config.remote_base_dir.clone();
            let dest = format!("{storage_backend}:{bucket}/{folder}/{name}");
            let config_location = self
                .config
                .rclone_config
                .as_ref()
                .map(|x| format!("--config={}", x))
                .unwrap_or("".to_string());
            let command = &["rclone", &config_location, "copyto", &orig, &dest];
            info!("Running: `{}`", command.join(" "));
            Command::new(command[0])
                .args(&command[1..])
                .spawn()
                .expect("command failed to start")
                .wait()
                .await
                .expect("command failed to run");
            res.push((storage_backend.to_string(), dest));
        }
        Ok(res)
    }

    // TODO: Consider using a struct instread of (String, String)
    pub async fn cp(
        &self,
        orig_folder: &str,
        orig_name: &str,
        dest_folder: &str,
        dest_name: &str,
    ) -> Result<Vec<(String, String)>, String> {
        let bucket = self.config.remote_base_dir.clone();

        let mut res = vec![];
        for storage_backend in self.storage_backends.iter() {
            let orig = format!("{storage_backend}:{bucket}/{orig_folder}/{orig_name}");
            let dest = format!("{storage_backend}:{bucket}/{dest_folder}/{dest_name}");
            let config_location = self
                .config
                .rclone_config
                .as_ref()
                .map(|x| format!("--config={}", x))
                .unwrap_or("".to_string());
            let command = &["rclone", &config_location, "copyto", &orig, &dest];
            info!("Running: `{}`", command.join(" "));
            Command::new(command[0])
                .args(&command[1..])
                .spawn()
                .expect("command failed to start")
                .wait()
                .await
                .expect("command failed to run");
            res.push((storage_backend.to_string(), dest));
        }
        Ok(res)
    }

    pub async fn rm(&self, folder: &str, file_name: &str) -> Result<Vec<(String, String)>, String> {
        let bucket = self.config.remote_base_dir.clone();

        let mut res = vec![];
        for storage_backend in self.storage_backends.iter() {
            let path = format!("{storage_backend}:{bucket}/{folder}/{file_name}");
            let config_location = self
                .config
                .rclone_config
                .as_ref()
                .map(|x| format!("--config={}", x))
                .unwrap_or("".to_string());
            let command = &["rclone", &config_location, "delete", &path];
            info!("Running: `{}`", command.join(" "));
            Command::new(command[0])
                .args(&command[1..])
                .spawn()
                .expect("command failed to start")
                .wait()
                .await
                .expect("command failed to run");
            res.push((storage_backend.to_string(), path));
        }
        Ok(res)
    }

    pub async fn rm_local_file(&self, folder: &str, name: &str) -> Result<(), String> {
        let orig = format!("{}/{}/{}", self.config.local_base_dir, folder, name);

        tokio::fs::remove_file(Path::new(&orig))
            .await
            .map_err(|e| format!("{}", e))?;
        Ok(())
    }

    // TODO: use a safer structure to hold Folder and name
    /// Remove the provided files.
    pub async fn free_read_cache(
        &self,
        removed_files: &Vec<(String, String)>,
    ) -> Result<(), String> {
        let rm_ops = removed_files.iter().map(|removed_file| {
            let file_path = format!(
                "{}/{}/{}",
                self.config.read_cache_dir, removed_file.0, removed_file.1
            );
            remove_file(file_path)
        });

        try_join_all(rm_ops)
            .await
            .map_err(|e| format!("Failed to remove file {:?}", e))?;

        Ok(())
    }

    pub async fn read_cached(
        &self,
        folder: &str,
        name: &str,
        base_offset: i64,
        size: u32,
    ) -> Result<Vec<u8>, String> {
        let file_path = format!("{}/{}/{}", self.config.read_cache_dir, folder, name);
        // TODO: better handle this failure
        let mut f = File::open(file_path).await.map_err(|e| e.to_string())?;
        let mut buffer = vec![0; size.try_into().unwrap()];

        f.seek(SeekFrom::Start(base_offset.try_into().unwrap()))
            .await
            .expect("Failed to seek");
        let mut total_read_bytes: usize = 0;
        while total_read_bytes < size.try_into().unwrap() {
            let read_bytes = f
                .read(&mut buffer)
                .await
                .map_err(|e| format!("Failed to read file {:?}", e))?;

            if read_bytes == 0 {
                break;
            }

            f.seek(SeekFrom::Current(read_bytes.try_into().unwrap()))
                .await
                .expect("Failed to seek");
            total_read_bytes += read_bytes;
        }

        Ok(buffer)
    }

    async fn run_rclone(&self, rclone_command: &[&str]) -> Result<ExitStatus, io::Error> {
        let config_location = self
            .config
            .rclone_config
            .as_ref()
            .map(|x| format!("--config={}", x))
            .unwrap_or("".to_string());
        let cmd = &[&["rclone", config_location.as_str()], rclone_command].concat();
        info!("Running: `{}`", cmd.join(" "));
        Command::new(cmd[0])
            .args(&cmd[1..])
            .spawn()
            .expect("command failed to start")
            .wait()
            .await
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
// // TODO: verify this is a reasonable size
// const BUF_SIZE: usize = 1 << 14; // 16 KiB
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
mod test {
    use crate::grizol;
    use crate::storage::StorageManager;
    use crate::syncthing::Request;
    use crate::GrizolConfig;

    #[tokio::test]
    async fn store_block_new_file_succeeds() {
        let config = grizol::Config {
            cert: "tests/util/config/cert-test.pem.key".to_string(),
            key: "tests/util/config/key-test.pem.key".to_string(),
            ..Default::default()
        };
        let bep_config = GrizolConfig::from(config);
        let storage_manager = StorageManager::new(bep_config);

        let data = vec![0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF];
        let request = Request {
            id: 0,
            folder: "test_dir".to_string(),
            name: "a_file".to_string(),
            offset: 0,
            size: data.len().try_into().unwrap(),
            hash: vec![],
            from_temporary: false,
        };

        let block_size: u64 = data.len().try_into().unwrap();
        let result = storage_manager
            .store_block(data, &request, block_size)
            .await;

        match result {
            Ok(_x) => assert_eq!(0, 0),
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
