use std::hash::{DefaultHasher, Hash, Hasher};
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::fs::{remove_file, OpenOptions};
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt, SeekFrom};
use tokio::sync::Mutex;

#[derive(Clone, Copy, Debug)]
enum FileWriterState {
    WritingChunks(usize),
    Consolidating,
}

// This is thread safe.
pub struct FileWriter {
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
    pub fn new(file_path: String, file_size: usize, chunk_size: usize) -> Self {
        let n_of_chunks = (file_size + chunk_size - 1) / chunk_size;
        let mut hasher = DefaultHasher::new();
        file_path.hash(&mut hasher);
        file_size.hash(&mut hasher);
        chunk_size.hash(&mut hasher);
        let suffix = hasher.finish().to_string();

        let mut writing_chunk: Vec<AtomicBool> = Default::default();
        for _ in 0..n_of_chunks {
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

    // Writes only the file the caller must ensure that the dir is valid.
    pub async fn write_chunk(&self, data: &[u8], offset: usize) -> io::Result<()> {
        debug!(
            "Start writing chunk for file {} at offset {}",
            &self.file_path, offset
        );

        // In case the file has zero size we just create it and we rely on the OS for syncing.
        if offset == 0 && self.file_size == 0 {
            OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(true)
                .open(&self.chunk_path(0))
                .await?;
            return Ok(());
        }

        if offset % self.chunk_size != 0 {
            let msg = format!(
                "Alignment is at multiples of {}, trying to write unaligned chunk at offset {} for file {}",
                self.chunk_size, offset, &self.file_path,
            );
            return Err(io::Error::new(io::ErrorKind::InvalidData, msg.as_str()));
        }

        if offset >= self.file_size {
            let msg = format!(
                "Trying to write at position {}, which is larger then the file {}",
                offset, self.file_size
            );
            return Err(io::Error::new(io::ErrorKind::InvalidData, msg.as_str()));
        }

        let last_offset = (self.file_size / self.chunk_size) * self.chunk_size;
        if data.len() != self.chunk_size && !(data.len() < self.chunk_size && offset == last_offset)
        {
            let msg = format!(
                "Wrong chunk size {} for file {} at offset {}",
                data.len(),
                self.file_path,
                offset
            );
            return Err(io::Error::new(io::ErrorKind::InvalidData, msg.as_str()));
        }

        {
            // This is expected to succeed
            let mut s = self.state.lock().await;
            match *s {
                FileWriterState::Consolidating => {
                    let msg = format!(
                        "Trying to write at position {}, which is larger then the file {}",
                        offset, self.file_size
                    );
                    return Err(io::Error::new(io::ErrorKind::Other, msg.as_str()));
                }
                FileWriterState::WritingChunks(x) => *s = FileWriterState::WritingChunks(x + 1),
            }
        }

        let chunk_pos = offset / self.chunk_size;
        // TODO: improve error handling
        if let Ok(true) = self.writing_chunk[chunk_pos].compare_exchange(
            false,
            true,
            Ordering::Acquire,
            Ordering::Relaxed,
        ) {
            let msg = format!(
                "chunk_pos {} for file {} is currently being written",
                chunk_pos, self.file_path
            );
            return Err(io::Error::new(io::ErrorKind::AlreadyExists, msg.as_str()));
        }

        let chunk = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(&self.chunk_path(chunk_pos))
            .await;
        let mut chunk = match chunk {
            Ok(x) => x,
            Err(e) => {
                self.writing_chunk[chunk_pos].store(false, Ordering::Relaxed);
                debug!("Error while writing {}", &self.chunk_path(chunk_pos));
                return Err(e);
            }
        };

        chunk.set_len(data.len() as u64).await?;
        chunk.write_all(data).await?;
        chunk.sync_all().await?;

        self.writing_chunk[chunk_pos].store(false, Ordering::Relaxed);

        {
            // This is expected to succeed
            let mut s = self.state.lock().await;
            if let FileWriterState::WritingChunks(x) = *s {
                *s = FileWriterState::WritingChunks(x - 1);
            } else {
                return Err(io::Error::new(
                    io::ErrorKind::Other,
                    "This should never happen, famous last words",
                ));
            }
        }

        debug!(
            "Finished writing chunk for file {} at offset {}",
            &self.file_path, offset
        );

        Ok(())
    }

    // Consolidate the chunks into a single file
    pub async fn consolidate(&self) -> io::Result<()> {
        {
            let mut s = self.state.lock().await;

            match *s {
                FileWriterState::Consolidating => {
                    return Err(io::Error::new(
                        io::ErrorKind::Other,
                        "Currently consolidating cannot further consolidate",
                    ))
                }
                FileWriterState::WritingChunks(1..) => {
                    return Err(io::Error::new(
                        io::ErrorKind::Other,
                        "Currently writing cannot consolidate",
                    ))
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
                .await?;
            for i in 0..self.writing_chunk.len() {
                {
                    let mut chunk = OpenOptions::new()
                        .read(true)
                        .write(false)
                        .create(false)
                        .truncate(false)
                        .open(&self.chunk_path(i))
                        .await?;
                    let mut buf: Vec<u8> = Vec::new();
                    chunk.read_to_end(&mut buf).await?;
                    file.seek(SeekFrom::Start((i * self.chunk_size) as u64))
                        .await?;
                    file.write_all(&buf).await?;
                    file.sync_all().await?;
                }
                remove_file(&self.chunk_path(i)).await?;
            }
        }

        {
            let mut s = self.state.lock().await;
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
    use std::sync::Arc;
    use std::thread;
    use tokio::fs::File;

    #[tokio::test]
    #[allow(clippy::non_snake_case)]
    async fn write_chunk__valid_input__succeeds() {
        let chunk_size = 2;
        let chunk_num = 26;
        let file_name = "/tmp/mynewfile".to_string();
        let mut file_writer: FileWriter =
            FileWriter::new(file_name.clone(), chunk_num * chunk_size, chunk_size);

        let mut chunks: Vec<usize> = (0..chunk_num).collect();
        chunks.shuffle(&mut thread_rng());
        for i in chunks.iter() {
            let data: Vec<u8> = vec![65 + (*i as u8); chunk_size];
            file_writer
                .write_chunk(&data, chunk_size * i)
                .await
                .unwrap();
        }

        file_writer.consolidate().await.unwrap();

        let mut f = File::open(file_name.as_str()).await.unwrap();
        let mut buf = String::default();
        f.read_to_string(&mut buf).await.unwrap();
        assert_eq!(
            buf.as_str(),
            "AABBCCDDEEFFGGHHIIJJKKLLMMNNOOPPQQRRSSTTUUVVWWXXYYZZ"
        );
        remove_file(file_name).await.unwrap();
    }

    #[tokio::test]
    #[allow(clippy::non_snake_case)]
    async fn write_chunk__valid_input_parallel__succeeds() {
        let chunk_size = 2;
        let chunk_num = 26;
        let file_name = "/tmp/mynewfile_parallel".to_string();
        let mut file_writer: FileWriter =
            FileWriter::new(file_name.clone(), chunk_num * chunk_size, chunk_size);

        let mut chunks: Vec<usize> = (0..chunk_num).collect();
        chunks.shuffle(&mut thread_rng());

        let fw = Arc::new(file_writer);
        let mut handles = Vec::new();
        for i in chunks.into_iter() {
            let data: Vec<u8> = vec![65 + (i as u8); chunk_size];

            let mut tmp = fw.clone();
            let handle = tokio::spawn(async move {
                tmp.write_chunk(&data, chunk_size * i).await.unwrap();
            });

            handles.push(handle);
        }

        // Wait for other threads to finish.
        for handle in handles.into_iter() {
            handle.await.unwrap();
        }

        fw.consolidate().await.unwrap();

        let mut f = File::open(file_name.as_str()).await.unwrap();
        let mut buf = String::default();
        f.read_to_string(&mut buf).await.unwrap();
        assert_eq!(
            buf.as_str(),
            "AABBCCDDEEFFGGHHIIJJKKLLMMNNOOPPQQRRSSTTUUVVWWXXYYZZ"
        );
        remove_file(file_name).await.unwrap();
    }
}
