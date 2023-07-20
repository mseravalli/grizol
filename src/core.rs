// Include the `syncthing` module, which is generated from syncthing.proto.
// It is important to maintain the same structure as in the proto.

mod bep_data_parser;

use crate::connectivity::OpenConnection;
use crate::core::bep_data_parser::{BepDataParser, CompleteMessage, MAGIC_NUMBER};
use crate::device_id::DeviceId;
use crate::storage;
use crate::storage::index_from_path;
use crate::syncthing;
use prost::Message;
use rand::prelude::*;
use std::array::TryFromSliceError;
use std::collections::HashSet;
use std::io::Write;
use std::path::Path;
use std::sync::mpsc::{Receiver, Sender};
use std::time::{Duration, Instant};
use syncthing::Header;
use syncthing::Hello;

const PING_INTERVAL: Duration = Duration::from_secs(45);

#[derive(Debug)]
pub enum BepAction {
    ReadClientMessage,
}

// TODO: maybe change name to something like BepConnectionHandler
pub struct BepProcessor {
    connection: OpenConnection,
    receiver: Receiver<BepAction>,
    // Last meaningful message
    last_message_sent_time: Option<Instant>,
    // TODO: think if we should have these in a ClientState struct
    local_id: DeviceId,
    index: syncthing::Index,
    client_id: HashSet<DeviceId>,
    data_parser: BepDataParser,
}

impl BepProcessor {
    pub fn new(
        local_id: DeviceId,
        connection: OpenConnection,
        receiver: Receiver<BepAction>,
        client_id: HashSet<DeviceId>,
    ) -> Self {
        let index = index_from_path(
            "test_a",
            Path::new("/home/marco/workspace/hic-sunt-leones/syncthing-test"),
            &local_id,
        )
        .unwrap();

        BepProcessor {
            connection,
            receiver,
            last_message_sent_time: None,
            local_id,
            client_id,
            index,
            data_parser: BepDataParser::new(),
        }
    }
    pub fn run(mut self) -> OpenConnection {
        loop {
            match self.receiver.recv_timeout(Duration::from_millis(100)) {
                Ok(action) => match action {
                    BepAction::ReadClientMessage => {
                        self.read_incoming_data();
                    }
                },
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                    // It is expected to get timeouts if e.g. no messages were sent.
                }
                Err(e) => {
                    error!("There was an error receiving the action: {}", e)
                }
            };

            if let Some(last_message_sent_time) = self.last_message_sent_time {
                if Instant::now().duration_since(last_message_sent_time) > PING_INTERVAL {
                    self.send_ping();
                    self.last_message_sent_time = Some(Instant::now());
                }
            }

            if self.connection.is_closed() {
                // TODO: add more info
                info!("Connection was closed");
                return self.connection;
            }
        }
    }

    fn read_incoming_data(&mut self) {
        let res = match self.connection.simplified_read() {
            Ok(buf) => {
                trace!("plaintext read {:#04x?}", &buf);
                self.data_parser
                    .process_incoming_data(&buf)
                    .map(|cm| self.handle_complete_messages(cm))
            }
            Err(e) => Err(format!("Received event was {}", e)),
        };
        match res {
            Err(e) => warn!("Encountered error when processing the data: {}", e),
            _ => {}
        }
    }

    fn handle_complete_messages(
        &mut self,
        complete_messages: Vec<CompleteMessage>,
    ) -> Result<(), String> {
        let res: Result<Vec<()>, String> = complete_messages
            .iter()
            .map(|complete_message| match complete_message {
                CompleteMessage::Hello(hello) => self.handle_hello(&hello),
                CompleteMessage::ClusterConfig(cluster_config) => {
                    self.handle_cluster_config(&cluster_config)
                }
                CompleteMessage::Index(index) => self.handle_index(&index),
                CompleteMessage::IndexUpdate(index_update) => todo!(),
                CompleteMessage::Request(request) => self.handle_request(&request),
                CompleteMessage::Response(response) => self.handle_response(&response),
                CompleteMessage::DownloadProgress(download_progress) => todo!(),
                CompleteMessage::Ping(ping) => self.handle_ping(&ping),
                CompleteMessage::Close(close) => self.handle_close(&close),
            })
            .collect();

        res.map(|_| ())
    }

    fn handle_hello(&mut self, hello: &syncthing::Hello) -> Result<(), String> {
        debug!("Handling Hello");
        self.send_hello()
            .and(self.send_cluster_config())
            .and(self.send_index())
    }

    fn handle_cluster_config(
        &mut self,
        cluster_config: &syncthing::ClusterConfig,
    ) -> Result<(), String> {
        debug!("Handling Cluster Config");
        Ok(())
    }

    fn handle_index(&mut self, index: &syncthing::Index) -> Result<(), String> {
        debug!("Handling Index");
        trace!("{:?}", index);
        let requests = self.update_index(index)?;
        let header = syncthing::Header {
            compression: 0,
            r#type: syncthing::MessageType::Response.into(),
        };
        for request in requests.into_iter() {
            self.send_message(header.clone(), request)?;
        }
        Ok(())
    }

    fn handle_request(&mut self, request: &syncthing::Request) -> Result<(), String> {
        debug!("Handling Request");
        debug!("{:?}", request);

        let request_folder = if request.folder == self.index.folder {
            Ok(&request.folder)
        } else {
            Err(syncthing::ErrorCode::Generic)
        };

        let file = request_folder.and_then(|x| {
            self.index
                .files
                .iter()
                .find(|&x| x.name == request.name)
                .ok_or(syncthing::ErrorCode::NoSuchFile)
        });

        let data = file.and_then(|f| {
            let block = f
                .blocks
                .iter()
                .find(|&b| {
                    b.offset == request.offset && b.size == request.size && b.hash == request.hash
                })
                .ok_or(syncthing::ErrorCode::NoSuchFile);

            block.and_then(|b| {
                storage::data_from_file_block(
                    // FIXME: track the dir somewhere else
                    "/home/marco/workspace/hic-sunt-leones/syncthing-test",
                    &f,
                    &b,
                )
                .map_err(|e| syncthing::ErrorCode::InvalidFile)
            })
        });

        let code: i32 = data
            .as_ref()
            .err()
            .unwrap_or(&syncthing::ErrorCode::NoError)
            .to_owned()
            .into();

        let response = syncthing::Response {
            id: request.id,
            data: data.unwrap_or(Vec::new()),
            code,
        };
        debug!("Sending Response");
        // debug!("Sending Response {:?}", response);

        let header = syncthing::Header {
            compression: 0,
            r#type: syncthing::MessageType::Response.into(),
        };
        self.send_message(header, response)
    }

    fn handle_response(&mut self, response: &syncthing::Response) -> Result<(), String> {
        debug!("Handling Response");
        trace!("{:?}", response);
        Ok(())
    }
    fn handle_ping(&mut self, ping: &syncthing::Ping) -> Result<(), String> {
        debug!("Handling Ping");
        trace!("{:?}", ping);
        Ok(())
    }

    fn handle_close(&mut self, close: &syncthing::Close) -> Result<(), String> {
        debug!("Handling Close");
        trace!("{:?}", close);
        Ok(())
    }

    fn send_hello(&mut self) -> Result<(), String> {
        let hello = syncthing::Hello {
            // TODO: read name from config file
            device_name: format!("testing_client"),
            client_name: format!("mydama"), // env!("CARGO_PKG_NAME").to_string(),
            client_version: format!("v0.0.2"), //env!("CARGO_PKG_VERSION").to_string(),
        };

        let message_bytes = hello.encode_to_vec();
        let message_len: u16 = message_bytes.len().try_into().unwrap();

        trace!("{:#04x?}", &hello.encode_to_vec());

        // TODO: use the right length
        let message: Vec<u8> = vec![]
            .into_iter()
            .chain(MAGIC_NUMBER.into_iter())
            .chain(message_len.to_be_bytes().into_iter())
            .chain(message_bytes.into_iter())
            .collect();

        debug!("Sending Hello");
        self.connection
            .write_all(&message)
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    fn send_cluster_config(&mut self) -> Result<(), String> {
        let header = syncthing::Header {
            compression: 0,
            r#type: syncthing::MessageType::ClusterConfig.into(),
        };

        let this_device = syncthing::Device {
            id: self.local_id.into(),
            name: format!("damorire"),
            addresses: vec![format!("127.0.0.1:23456")],
            compression: syncthing::Compression::Never.into(),
            max_sequence: 100,
            // Delta Index Exchange is not supported yet hence index_id is zero.
            index_id: 0,
            cert_name: String::new(),
            encryption_password_token: vec![],
            introducer: false,
            skip_introduction_removals: true,
            // ..Default::default()
        };

        // FIXME: This is an ugly hack to get the client id fix the workflow.
        let client_id = self.client_id.iter().next().unwrap();
        let client_device = syncthing::Device {
            id: client_id.into(),
            name: format!("syncthing"),
            addresses: vec![format!("127.0.0.1:220000")],
            compression: syncthing::Compression::Never.into(),
            max_sequence: 100,
            // Delta Index Exchange is not supported yet hence index_id is zero.
            index_id: 0,
            cert_name: String::new(),
            encryption_password_token: vec![],
            introducer: false,
            skip_introduction_removals: true,
            // ..Default::default()
        };

        let folder_name = "test_a";

        let folder = syncthing::Folder {
            id: folder_name.to_string(),
            label: folder_name.to_string(),
            read_only: false,
            ignore_permissions: false,
            ignore_delete: true,
            disable_temp_indexes: false,
            paused: false,
            devices: vec![this_device, client_device],
            // ..Default::default()
        };

        let mut cluster_config = syncthing::ClusterConfig {
            folders: vec![folder],
        };

        debug!("Sending Cluster Config");
        self.send_message(header, cluster_config)
    }

    fn send_index(&mut self) -> Result<(), String> {
        let header = syncthing::Header {
            compression: 0,
            r#type: syncthing::MessageType::Index.into(),
        };

        debug!("Sending Index");
        trace!("Sending Index: {:?}", &self.index);
        self.send_message(header, self.index.clone())
    }

    fn send_ping(&mut self) -> Result<(), String> {
        let header = syncthing::Header {
            r#type: syncthing::MessageType::Ping.into(),
            compression: 0,
        };

        let ping = syncthing::Ping::default();

        self.send_message(header, ping)
    }

    fn update_index(
        &mut self,
        index: &syncthing::Index,
    ) -> Result<Vec<syncthing::Request>, String> {
        todo!();
    }

    // FIXME: don't use index this way, read it from self or something
    // TODO: fix returns
    // fn request_files(&mut self, index: &syncthing::Index) -> Result<(), String> {
    //     let header = syncthing::Header {
    //         compression: 0,
    //         r#type: syncthing::MessageType::Request.into(),
    //     };

    //     if index.folder.starts_with("test_") {
    //         return Ok(());
    //     }

    //     for file in index.files.iter() {
    //         if file.name != "EBPLEATKTYXKCPEASMCJ" {
    //             continue;
    //         }
    //         for block in file.blocks.iter() {
    //             let id = rand::random::<i32>().abs();
    //             let request = syncthing::Request {
    //                 id,
    //                 folder: index.folder.clone(),
    //                 name: file.name.clone(),
    //                 offset: block.offset,
    //                 size: block.size,
    //                 hash: block.hash.clone(),
    //                 from_temporary: false,
    //             };
    //             debug!("Sending Request");
    //             trace!("Sending Request {:?}", request);
    //             self.send_message(header.clone(), request);
    //         }
    //     }
    //     Ok(())
    // }

    fn handle_download_progress(&self, raw_message: &[u8]) {
        debug!("Handling DownloadProgress");

        match syncthing::DownloadProgress::decode(raw_message) {
            Ok(download_progress) => {
                debug!("{:?}", download_progress);
            }
            Err(e) => {
                error!("Error while decoding {:?}", e);
            }
        }
    }

    fn send_message<T: prost::Message>(
        &mut self,
        header: syncthing::Header,
        message: T,
    ) -> Result<(), String> {
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

        let res = self
            .connection
            .write_all(&message)
            .map_err(|e| e.to_string())
            .and_then(|written_bytes| {
                let total_bytes_to_be_written: usize = (message_len + header_len as u32 + 2 + 4)
                    .try_into()
                    .unwrap();
                trace!(
                    "written bytes {}, total_bytes_to_be_written {}",
                    written_bytes,
                    total_bytes_to_be_written
                );
                if written_bytes < total_bytes_to_be_written {
                    Err(format!(
                        "written {} out of {} bytes",
                        written_bytes, total_bytes_to_be_written
                    ))
                } else {
                    Ok(())
                }
            });

        trace!("Sending res: {:?}", res);
        res
    }
}
