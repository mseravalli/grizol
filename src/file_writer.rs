use rand::distributions::{Alphanumeric, DistString};
use rand::{thread_rng, Rng};
use std::sync::atomic::{AtomicBool, Ordering};
use std::{
    fs::{File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::FileExt,
};

struct FileWriter {
    file_path: String,
    file_size: usize,
    chunk_size: usize,
    // use an atomic bool or something
    written_chunks: Vec<bool>,
    // We want to have a random suffix to add at the end of the chunks to avoid possible
    // concurrency issues.
    random_suff: String,
}

impl FileWriter {
    fn new(file_path: String, file_size: usize, chunk_size: usize) -> Self {
        let n_of_chunks = (file_size + chunk_size - 1) / chunk_size;
        // The probability of hitting a match should be 1/(62^8).
        let random_suff = Alphanumeric.sample_string(&mut rand::thread_rng(), 8);

        FileWriter {
            file_path,
            file_size,
            chunk_size,
            written_chunks: vec![false; n_of_chunks],
            random_suff,
        }
    }

    fn chunk_path(&self, chunk_pos: usize) -> String {
        format!("{}.part_{}.{}", self.file_path, chunk_pos, self.random_suff)
    }

    fn write_chunk(&mut self, data: &[u8], offset: usize) -> Result<(), String> {
        if offset % self.chunk_size != 0 {
            return Err(format!(
                "Alignment is at multiples of {}, trying to write unaligned chunk at offset {} for file {}",
                self.chunk_size, offset, &self.file_path,
            ));
        }

        if data.len() != self.chunk_size
            && !(data.len() < self.chunk_size && offset == self.file_size / self.chunk_size)
        {
            return Err(format!(
                "Wrong chunk size {} for file {} at offset {}",
                data.len(),
                self.file_path,
                offset
            ));
        }

        let chunk_pos = offset / self.chunk_size;
        if self.written_chunks[chunk_pos] {
            return Err(format!(
                "chunk_pos {} for file {} was already written",
                chunk_pos, self.file_path
            ));
        }

        let chunk = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&self.chunk_path(chunk_pos));
        let mut chunk = match chunk {
            Ok(x) => x,
            Err(e) => {
                self.written_chunks[chunk_pos] = false;
                return Err(format!(
                    "Could not write chunk at offset {} for file {}",
                    offset, &self.file_path
                ));
            }
        };

        chunk.set_len(data.len() as u64);
        chunk.write_all(data);
        chunk.flush();

        self.written_chunks[chunk_pos] = true;

        // consolidate the chunks into a single file
        if self.written_chunks.iter().all(|&x| x) {
            let mut file = OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(true)
                .open(&self.file_path)
                .unwrap();

            for i in (0..self.written_chunks.len()) {
                {
                    let mut chunk = OpenOptions::new()
                        .read(true)
                        .write(false)
                        .create(false)
                        .truncate(false)
                        .open(&self.chunk_path(i))
                        .unwrap();
                    let mut buf: Vec<u8> = Vec::new();
                    chunk.read_to_end(&mut buf);
                    file.write_all_at(&buf, (i * self.chunk_size) as u64);
                }

                std::fs::remove_file(&self.chunk_path(i)).unwrap();
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn write_chunk__valid_input__succeeds() {
        let chunk_size = 2;
        let mut file_writer: FileWriter =
            FileWriter::new("/tmp/mynewfile".to_string(), 20 * chunk_size, chunk_size);

        for i in (0..20) {
            let data: Vec<u8> = vec![65 + i as u8; chunk_size];
            file_writer.write_chunk(&data, chunk_size * i);
        }
        assert_eq!(1, 1);
    }
}
