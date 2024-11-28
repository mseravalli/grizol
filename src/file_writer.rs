use futures::io::Flush;
use rand::distributions::{Alphanumeric, DistString};
use rand::{thread_rng, Rng};
use std::borrow::BorrowMut;
use std::ops::DerefMut;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::{
    fs::{File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::FileExt,
};

#[derive(Clone, Copy)]
enum FileWriterState {
    WritingChunks(usize),
    Consolidating,
}

struct FileWriter {
    file_path: String,
    file_size: usize,
    chunk_size: usize,
    // use an atomic bool or something
    writing_chunk: Vec<AtomicBool>,
    // We want to have a suffix to add at the end of the chunks to avoid possible concurrency
    // issues, with existing files.
    suffix: String,
    state: Mutex<FileWriterState>,
}

unsafe impl std::marker::Sync for FileWriter {}
unsafe impl std::marker::Send for FileWriter {}

impl FileWriter {
    fn new(file_path: String, file_size: usize, chunk_size: usize) -> Self {
        let n_of_chunks = (file_size + chunk_size - 1) / chunk_size;
        // The probability of hitting a match should be 1/(62^8).
        // TODO: use the hash of the filename
        let suffix = Alphanumeric.sample_string(&mut rand::thread_rng(), 8);

        let mut writing_chunk: Vec<AtomicBool> = Default::default();
        for i in (0..n_of_chunks) {
            writing_chunk.push(AtomicBool::new(false))
        }

        FileWriter {
            file_path,
            file_size,
            chunk_size,
            writing_chunk,
            suffix,
            state: Mutex::new(FileWriterState::WritingChunks(0)),
        }
    }

    fn chunk_path(&self, chunk_pos: usize) -> String {
        format!("{}.part_{}.{}", self.file_path, chunk_pos, self.suffix)
    }

    fn write_chunk(&self, data: &[u8], offset: usize) -> Result<(), String> {
        if offset % self.chunk_size != 0 {
            return Err(format!(
                "Alignment is at multiples of {}, trying to write unaligned chunk at offset {} for file {}",
                self.chunk_size, offset, &self.file_path,
            ));
        }

        if offset >= self.file_size {
            return Err(format!(
                "Trying to write at position {}, which is larger then the file {}",
                offset, self.file_size
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

        {
            let mut s = self
                .state
                .lock()
                .map_err(|x| format!("Error getting the lock: {}", x))?;
            match *s {
                FileWriterState::Consolidating => {
                    return Err("Currently consolidating cannot write data".to_string())
                }
                FileWriterState::WritingChunks(x) => *s = FileWriterState::WritingChunks(x + 1),
            }
        }

        let chunk_pos = offset / self.chunk_size;
        // TODO: improve error handling
        if let (Ok(true)) = self.writing_chunk[chunk_pos].compare_exchange(
            false,
            true,
            Ordering::Acquire,
            Ordering::Relaxed,
        ) {
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
                self.writing_chunk[chunk_pos].store(false, Ordering::Relaxed);
                return Err(format!(
                    "Could not write chunk at offset {} for file {}",
                    offset, &self.file_path
                ));
            }
        };

        chunk.set_len(data.len() as u64);
        chunk.write_all(data);
        chunk.flush();

        self.writing_chunk[chunk_pos].store(false, Ordering::Relaxed);

        {
            let mut s = self
                .state
                .lock()
                .map_err(|x| format!("Error getting the lock: {}", x))?;
            if let FileWriterState::WritingChunks(x) = *s {
                *s = FileWriterState::WritingChunks(x - 1);
            } else {
                return Err("This should never happen, famous last words".to_string());
            }
        }

        Ok(())
    }

    // consolidate the chunks into a single file
    fn consolidate(&self) -> Result<(), String> {
        {
            let mut s = self
                .state
                .lock()
                .map_err(|x| format!("Error getting the lock: {}", x))?;
            match *s {
                FileWriterState::Consolidating => {
                    return Err("Currently consolidating cannot further consolidate".to_string())
                }
                FileWriterState::WritingChunks(1..) => {
                    return Err("Currently writing cannot consolidate".to_string())
                }
                FileWriterState::WritingChunks(0) => *s = FileWriterState::Consolidating,
            }
        }

        // there should not be any writing happening this should be redundat to the mutex above
        if self
            .writing_chunk
            .iter()
            .all(|x| !x.load(Ordering::Relaxed))
        {
            let mut file = OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(true)
                .open(&self.file_path)
                .map_err(|x| format!("Error opening the file: {}", x))?;
            for i in (0..self.writing_chunk.len()) {
                {
                    let mut chunk = OpenOptions::new()
                        .read(true)
                        .write(false)
                        .create(false)
                        .truncate(false)
                        .open(&self.chunk_path(i))
                        .map_err(|x| format!("Error opening the file: {}", x))?;
                    let mut buf: Vec<u8> = Vec::new();
                    chunk.read_to_end(&mut buf);
                    file.write_all_at(&buf, (i * self.chunk_size) as u64);
                }
                std::fs::remove_file(&self.chunk_path(i))
                    .map_err(|x| format!("Error removing the file: {}", x))?;
            }
        }

        {
            let mut s = self
                .state
                .lock()
                .map_err(|x| format!("Error getting the lock: {}", x))?;
            *s = FileWriterState::WritingChunks(0)
        }

        Ok(())
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use rand::prelude::SliceRandom;
    use rand::{thread_rng, Rng};
    use std::borrow::BorrowMut;
    use std::fs;
    use std::sync::Arc;
    use std::thread;

    #[test]
    #[allow(clippy::non_snake_case)]
    fn write_chunk__valid_input__succeeds() {
        let chunk_size = 2;
        let chunk_num = 26;
        let file_name = "/tmp/mynewfile".to_string();
        let mut file_writer: FileWriter =
            FileWriter::new(file_name.clone(), chunk_num * chunk_size, chunk_size);

        let mut chunks: Vec<usize> = (0..chunk_num).collect();
        chunks.shuffle(&mut thread_rng());
        for i in chunks.iter() {
            let data: Vec<u8> = vec![65 + (*i as u8); chunk_size];
            file_writer.write_chunk(&data, chunk_size * i);
        }

        file_writer.consolidate();

        let mut f = File::open(file_name.as_str()).unwrap();
        let mut buf = String::default();
        f.read_to_string(&mut buf);
        println!("{}", buf);
        assert_eq!(
            buf.as_str(),
            "AABBCCDDEEFFGGHHIIJJKKLLMMNNOOPPQQRRSSTTUUVVWWXXYYZZ"
        );
        fs::remove_file(file_name).unwrap();
    }

    #[test]
    #[allow(clippy::non_snake_case)]
    fn write_chunk__valid_input_parallel__succeeds() {
        let chunk_size = 2;
        let chunk_num = 26;
        let file_name = "/tmp/mynewfile".to_string();
        let mut file_writer: FileWriter =
            FileWriter::new(file_name.clone(), chunk_num * chunk_size, chunk_size);

        let mut chunks: Vec<usize> = (0..chunk_num).collect();
        chunks.shuffle(&mut thread_rng());

        let fw = Arc::new(file_writer);
        let mut handles = Vec::new();
        for i in chunks.into_iter() {
            let data: Vec<u8> = vec![65 + (i as u8); chunk_size];

            let mut tmp = fw.clone();
            let handle = thread::spawn(move || {
                tmp.write_chunk(&data, chunk_size * i);
            });

            handles.push(handle);
        }

        // Wait for other threads to finish.
        for handle in handles {
            handle.join().unwrap();
        }

        fw.consolidate();

        let mut f = File::open(file_name.as_str()).unwrap();
        let mut buf = String::default();
        f.read_to_string(&mut buf);
        println!("{}", buf);
        assert_eq!(
            buf.as_str(),
            "AABBCCDDEEFFGGHHIIJJKKLLMMNNOOPPQQRRSSTTUUVVWWXXYYZZ"
        );
        fs::remove_file(file_name).unwrap();
    }
}
