// Include the `syncthing` module, which is generated from syncthing.proto.
// It is important to maintain the same structure as in the proto.

pub mod bep_data_parser;

use crate::connectivity::OpenConnection;
use crate::core::bep_data_parser::{BepDataParser, CompleteMessage, MAGIC_NUMBER};
use crate::device_id::DeviceId;
use crate::storage;
use crate::storage::index_from_path;
use crate::syncthing;
use prost::Message;
use rand::prelude::*;
use std::array::TryFromSliceError;
use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::io::Write;
use std::path::Path;
use std::pin::Pin;
use std::sync::Mutex;
use std::time::{Duration, Instant};

const PING_INTERVAL: Duration = Duration::from_secs(45);

#[derive(Debug, Clone)]
pub struct EncodedMessage {
    data: Vec<u8>,
}

impl EncodedMessage {
    fn new(data: Vec<u8>) -> Self {
        EncodedMessage { data }
    }

    pub fn data(&self) -> &[u8] {
        &self.data[..]
    }
}

// TODO: initialize through a from config or something, it's probably easier
pub struct BepConfig {
    pub id: DeviceId,
    pub name: String,
    pub trusted_peers: HashSet<DeviceId>,
}

struct BepState {
    indexes: HashMap<String, syncthing::Index>,
    cluster: syncthing::ClusterConfig,
}

type FutureEncodedMessage<'a> = Pin<Box<dyn Future<Output = EncodedMessage> + Send + 'a>>;

// TODO: maybe change name to something like BepConnectionHandler
pub struct BepProcessor {
    config: BepConfig,
    state: Mutex<BepState>,
}

impl BepProcessor {
    pub fn new(config: BepConfig) -> Self {
        let index = index_from_path(
            "test_a",
            Path::new("/home/marco/workspace/hic-sunt-leones/syncthing-test"),
            &config.id,
        )
        .unwrap();

        let bep_state = BepState {
            indexes: Default::default(),
            cluster: syncthing::ClusterConfig { folders: vec![] },
        };

        BepProcessor {
            config,
            state: Mutex::new(bep_state),
        }
    }

    pub fn handle_complete_message(
        &self,
        complete_message: CompleteMessage,
    ) -> Vec<FutureEncodedMessage> {
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

    fn handle_hello(&self, hello: syncthing::Hello) -> Vec<FutureEncodedMessage> {
        debug!("Handling Hello");
        vec![Box::pin(self.hello()), Box::pin(self.cluster_config())]
        // let messages = vec![self.hello()?, self.cluster_config()?, self.index()?];
    }

    fn handle_cluster_config(
        &self,
        cluster_config: syncthing::ClusterConfig,
    ) -> Vec<FutureEncodedMessage> {
        debug!("Handling Cluster Config");
        vec![]
    }

    fn handle_index_update(
        &self,
        index_update: syncthing::IndexUpdate,
    ) -> Vec<FutureEncodedMessage> {
        let index = syncthing::Index {
            folder: index_update.folder,
            files: index_update.files,
        };
        self.handle_index(index)
    }

    fn handle_index(&self, index: syncthing::Index) -> Vec<FutureEncodedMessage> {
        debug!("Handling Index");
        vec![]

        // trace!("{:?}", index);
        // let requests = self.update_index(index)?;
        // let header = syncthing::Header {
        //     compression: 0,
        //     r#type: syncthing::MessageType::Response.into(),
        // };
        // requests
        //     .into_iter()
        //     .map(|request| self.encode_message(header.clone(), request))
        //     .collect()
    }

    fn handle_request(&self, request: syncthing::Request) -> Vec<FutureEncodedMessage> {
        debug!("Handling Request");
        debug!("{:?}", request);

        vec![]

        // let request_folder = if request.folder == self.index.folder {
        //     Ok(&request.folder)
        // } else {
        //     Err(syncthing::ErrorCode::Generic)
        // };

        // let file = request_folder.and_then(|x| {
        //     self.index
        //         .files
        //         .iter()
        //         .find(|&x| x.name == request.name)
        //         .ok_or(syncthing::ErrorCode::NoSuchFile)
        // });

        // let data = file.and_then(|f| {
        //     let block = f
        //         .blocks
        //         .iter()
        //         .find(|&b| {
        //             b.offset == request.offset && b.size == request.size && b.hash == request.hash
        //         })
        //         .ok_or(syncthing::ErrorCode::NoSuchFile);

        //     block.and_then(|b| {
        //         storage::data_from_file_block(
        //             // FIXME: track the dir somewhere else
        //             "/home/marco/workspace/hic-sunt-leones/syncthing-test",
        //             &f,
        //             &b,
        //         )
        //         .map_err(|e| syncthing::ErrorCode::InvalidFile)
        //     })
        // });

        // let code: i32 = data
        //     .as_ref()
        //     .err()
        //     .unwrap_or(&syncthing::ErrorCode::NoError)
        //     .to_owned()
        //     .into();

        // let response = syncthing::Response {
        //     id: request.id,
        //     data: data.unwrap_or(Vec::new()),
        //     code,
        // };
        // debug!("Sending Response");
        // // debug!("Sending Response {:?}", response);

        // let header = syncthing::Header {
        //     compression: 0,
        //     r#type: syncthing::MessageType::Response.into(),
        // };
        // vec![Box::pin(self.encode_message(header, response).unwrap())]
    }

    fn handle_response(&self, response: syncthing::Response) -> Vec<FutureEncodedMessage> {
        debug!("Handling Response");
        trace!("{:?}", response);
        vec![]
    }

    fn handle_ping(&self, ping: syncthing::Ping) -> Vec<FutureEncodedMessage> {
        debug!("Handling Ping");
        trace!("{:?}", ping);
        vec![]
    }

    fn handle_close(&self, close: syncthing::Close) -> Vec<FutureEncodedMessage> {
        debug!("Handling Close");
        trace!("{:?}", close);
        vec![]
    }

    async fn hello(&self) -> EncodedMessage {
        let hello = syncthing::Hello {
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

        EncodedMessage::new(message)
    }

    async fn cluster_config(&self) -> EncodedMessage {
        let header = syncthing::Header {
            compression: 0,
            r#type: syncthing::MessageType::ClusterConfig.into(),
        };

        let this_device = syncthing::Device {
            id: self.config.id.into(),
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
        let client_id = self.config.trusted_peers.iter().next().unwrap();
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
        self.encode_message(header, cluster_config).unwrap()
    }

    // fn index(&mut self) -> Result<EncodedMessage, String> {
    //     let header = syncthing::Header {
    //         compression: 0,
    //         r#type: syncthing::MessageType::Index.into(),
    //     };

    //     debug!("Sending Index");
    //     self.encode_message(header, self.index.clone())
    // }

    fn update_index(&mut self, index: syncthing::Index) -> Result<Vec<syncthing::Request>, String> {
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

    fn encode_message<T: prost::Message>(
        &self,
        header: syncthing::Header,
        message: T,
    ) -> Result<EncodedMessage, String> {
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

        Ok(EncodedMessage { data: message })
    }
}
