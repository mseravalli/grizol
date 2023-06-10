use sha2::{Digest, Sha256};
use std::fs::File;
use std::io::prelude::*;
use std::io::BufReader;
use std::path::Path;

pub mod syncthing {
    include!(concat!(env!("OUT_DIR"), "/syncthing.rs"));
}

// TODO: verify this is a reasonable size
const BUF_SIZE: usize = 2 << 14; // 16 KiB

pub fn hash_file_blocks(
    path: &Path,
    block_size: usize,
) -> Result<Vec<syncthing::BlockInfo>, std::io::Error> {
    let file = File::open(path)?;
    let mut file_reader = BufReader::new(file);
    let mut buf: [u8; BUF_SIZE] = [0; BUF_SIZE];
    let mut res: Vec<syncthing::BlockInfo> = Default::default();

    if BUF_SIZE >= block_size {
        let mut offset: i64 = 0;
        loop {
            let read_data_len = file_reader.read(&mut buf)?;
            if read_data_len == 0 {
                break;
            }
            for i in 0..((read_data_len + block_size - 1) / block_size) {
                let start = i * block_size;
                let end = std::cmp::min((i + 1) * block_size, read_data_len);
                let hasher_sha256 = Sha256::new_with_prefix(&buf[start..end]);
                let block = syncthing::BlockInfo {
                    offset,
                    size: (end - start) as i32,
                    hash: hasher_sha256.finalize().to_vec(),
                    weak_hash: 0, // FIXME
                };
                offset += 1;
                res.push(block);
            }
        }
    } else {
        let mut pushed_to_hasher: usize = 0;
        let mut offset: i64 = 0;
        let mut hasher_sha256 = Sha256::new();

        loop {
            let read_data_len = file_reader.read(&mut buf)?;
            if read_data_len == 0 {
                break;
            }
            if pushed_to_hasher + read_data_len < block_size {
                hasher_sha256.update(&buf[..read_data_len]);
                pushed_to_hasher += read_data_len;
            } else {
                let to_add_to_hasher = block_size - pushed_to_hasher;
                hasher_sha256.update(&buf[..to_add_to_hasher]);
                pushed_to_hasher += to_add_to_hasher;

                let block = syncthing::BlockInfo {
                    offset: offset,
                    size: pushed_to_hasher as i32,
                    hash: hasher_sha256.finalize_reset().to_vec(),
                    weak_hash: 0, // FIXME
                };
                offset += 1;
                res.push(block);

                hasher_sha256.update(&buf[to_add_to_hasher..]);
                pushed_to_hasher = read_data_len - to_add_to_hasher;
            }
        }

        if pushed_to_hasher > 0 {
            let block = syncthing::BlockInfo {
                offset: offset,
                size: pushed_to_hasher as i32,
                hash: hasher_sha256.finalize().to_vec(),
                weak_hash: 0, // FIXME
            };
            offset += 1;
            res.push(block);
        }
    }

    Ok(res)
}

#[cfg(test)]
mod tests {
    use crate::storage::hash_file_blocks;
    use crate::storage::syncthing;
    use std::path::Path;

    fn file_path() -> String {
        "/home/marco/test_000/test_1".to_string()
    }
    #[test]
    fn hash_file_blocks_buf_size_lt_block_size() {
        let block_size = 2 << 16; // 128 KiB

        let blocks: Vec<syncthing::BlockInfo> =
            hash_file_blocks(Path::new(&file_path()), block_size).unwrap();
        println!("{:?}", blocks);

        let blocks_control = vec![syncthing::BlockInfo {
            offset: 0,
            size: 10,
            // This is the sha256sum of
            hash: vec![
                0x9a, 0x89, 0xc6, 0x8c, 0x4c, 0x5e, 0x28, 0xb8, 0xc4, 0xa5, 0x56, 0x76, 0x73, 0xd4,
                0x62, 0xff, 0xf5, 0x15, 0xdb, 0x46, 0x11, 0x6f, 0x99, 0x00, 0x62, 0x4d, 0x09, 0xc4,
                0x74, 0xf5, 0x93, 0xfb,
            ],
            weak_hash: 0,
        }];
        println!("{:?}", blocks_control);
        assert!(blocks == blocks_control);
    }
}
