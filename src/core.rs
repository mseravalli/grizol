// Include the `syncthing` module, which is generated from syncthing.proto.
// It is important to maintain the same structure as in the proto.
use crate::connectivity::OpenConnection;
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
const MAGIC_NUMBER: [u8; 4] = [0x2e, 0xa7, 0xd9, 0x0b];
const HELLO_LEN_START: usize = 4;
const HELLO_START: usize = 6;
const HEADER_START: usize = 2;

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
enum IncomingMessageStatus {
    Incomplete,
    Complete,
}

impl Default for IncomingMessageStatus {
    fn default() -> Self {
        IncomingMessageStatus::Incomplete
    }
}

#[derive(Debug)]
struct IncomingMessage {
    auth_status: BepAuthStatus,
    data: Vec<u8>,
    header_byte_len: Option<usize>,
    header: Option<syncthing::Header>,
    message_byte_len: Option<usize>,
}

impl IncomingMessage {
    fn new(auth_status: BepAuthStatus) -> Self {
        IncomingMessage {
            auth_status,
            data: Default::default(),
            header_byte_len: Default::default(),
            header: Default::default(),
            message_byte_len: Default::default(),
        }
    }
    fn status(&self) -> IncomingMessageStatus {
        match self.auth_status {
            BepAuthStatus::PreHello => self
                .message_byte_len
                .map(|mbl| {
                    if self.data.len() == HELLO_START + mbl {
                        IncomingMessageStatus::Complete
                    } else {
                        IncomingMessageStatus::Incomplete
                    }
                })
                .unwrap_or(IncomingMessageStatus::Incomplete),
            BepAuthStatus::PostHello => {
                todo!()
            }
        }
    }
    fn total_byte_len(&self) -> usize {
        self.data.len()
    }
    fn add_data(&mut self, buf: &[u8]) {
        self.data.extend_from_slice(&buf);

        match self.auth_status {
            BepAuthStatus::PreHello => {
                if self.data.len() >= HELLO_START && self.message_byte_len.is_none() {
                    self.message_byte_len = Some(
                        u16::from_be_bytes(
                            self.data[HELLO_LEN_START..HELLO_START].try_into().unwrap(),
                        )
                        .into(),
                    );
                }
            }
            BepAuthStatus::PostHello => {
                if self.header.is_none() {
                    self.header = try_extract_header(&self.data).ok();
                } else {
                    todo!()
                }
            }
        };
    }
    fn header_byte_len(&self) -> Option<usize> {
        if self.data.len() < HEADER_START {
            return None;
        }

        let header_byte_len: usize =
            u16::from_be_bytes(self.data[0..HEADER_START].try_into().unwrap()).into();
        Some(header_byte_len)
    }
    fn missing_header_bytes(&self) -> Option<usize> {
        let missing_bytes = self
            .header_byte_len()
            .map(|header_byte_len| HEADER_START + header_byte_len + self.total_byte_len());

        missing_bytes.map(|x| assert!(x >= 0));
        missing_bytes
    }
    fn missing_message_bytes(&self) -> Option<usize> {
        match self.auth_status {
            BepAuthStatus::PreHello => {
                let missing_bytes = self
                    .message_byte_len
                    .map(|mbl| mbl + HELLO_START - self.data.len());
                missing_bytes.map(|x| assert!(x >= 0));
                missing_bytes
            }
            BepAuthStatus::PostHello => {
                todo!()
            }
        }
        // trace!(
        //     "type {:?} - message_byte_size {} - data.len() {} ",
        //     self.header.as_ref().map(|x| x.r#type),
        //     self.message_byte_size,
        //     self.data.len()
        // );
        // let missing_bytes = self.message_byte_size - self.data.len();
        // assert!(missing_bytes >= 0);
        // missing_bytes
    }
}

// impl TryInto<syncthing::Hello> for &IncomingMessage {
//     type Error = String;
//     fn try_into(self) -> Result<syncthing::Hello, Self::Error> {
//         let message_byte_size: usize = u16::from_be_bytes(
//             self.data[HELLO_LEN_START..HELLO_START]
//                 .try_into()
//                 .map_err(|e| format!("Failed with {:?}", e))?,
//         )
//         .into();

//         syncthing::Hello::decode(&self.data[HELLO_START..HELLO_START + message_byte_size])
//             .map_err(|e| format!("Failed with {:?}", e))
//     }
// }

#[derive(Debug, PartialEq)]
enum CompleteMessage {
    Hello(syncthing::Hello),
    ClusterConfig(syncthing::ClusterConfig),
    Index(syncthing::Index),
}

impl TryFrom<&IncomingMessage> for CompleteMessage {
    type Error = String;
    fn try_from(input: &IncomingMessage) -> Result<Self, Self::Error> {
        match input.auth_status {
            BepAuthStatus::PreHello => {
                if !starts_with_magic_number(&input.data) {
                    return Err(format!("Hello message does not start with magic number"));
                }
                let message_byte_size: usize = u16::from_be_bytes(
                    input.data[HELLO_LEN_START..HELLO_START].try_into().unwrap(),
                )
                .into();
                let hello = syncthing::Hello::decode(
                    &input.data[HELLO_START..HELLO_START + message_byte_size],
                )
                .unwrap();
                Ok(CompleteMessage::Hello(hello))
            }
            BepAuthStatus::PostHello => {
                todo!()
            }
        }
    }
}

#[derive(Debug)]
pub enum BepAction {
    ReadClientMessage,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BepAuthStatus {
    PreHello,
    PostHello,
}

#[derive(Debug)]
struct BepDataProcessor {
    bep_auth_status: BepAuthStatus,
    incoming_message: Option<IncomingMessage>,
}

impl BepDataProcessor {
    fn new() -> Self {
        BepDataProcessor {
            bep_auth_status: BepAuthStatus::PreHello,
            incoming_message: None,
        }
    }
    fn process_incoming_data(&mut self, buf: &[u8]) -> Result<Vec<CompleteMessage>, String> {
        let mut processed_bytes: usize = 0;
        let mut res: Vec<CompleteMessage> = Default::default();
        while processed_bytes < buf.len() {
            match self.bep_auth_status {
                BepAuthStatus::PreHello => {
                    processed_bytes += self.populate_hello(&buf[processed_bytes..])?;
                }
                BepAuthStatus::PostHello => {
                    let is_header_missing = self
                        .incoming_message
                        .as_ref()
                        .map(|im| im.header.as_ref())
                        .flatten()
                        .is_none();
                    if is_header_missing {
                        processed_bytes += self.populate_header(&buf[processed_bytes..])?;
                    } else {
                    }
                }
            };

            let complete_message = match self.incoming_message.as_ref().map(|im| (im, im.status()))
            {
                Some((x, IncomingMessageStatus::Complete)) => {
                    let complete_message: Result<CompleteMessage, String> = x.try_into();
                    if self.bep_auth_status == BepAuthStatus::PreHello {
                        self.bep_auth_status = BepAuthStatus::PostHello;
                    }
                    complete_message
                }
                _ => Err(format!("Message was not complete yet")),
            };

            if let Ok(cm) = complete_message {
                res.push(cm);
                self.incoming_message = None;
            }
        }
        Ok(res)
    }

    fn populate_hello(&mut self, buf: &[u8]) -> Result<usize, String> {
        let mut processed_bytes = 0;

        if let Some(ref mut im) = self.incoming_message.as_mut() {
            assert!(im.total_byte_len() >= 1);
            if im.total_byte_len() < HELLO_START {
                let available_for_hello =
                    std::cmp::min(buf.len(), HELLO_START - im.total_byte_len());
                im.add_data(&buf[..available_for_hello]);
                processed_bytes += available_for_hello;
            } else {
                let are_bytes_missing = self
                    .incoming_message
                    .as_ref()
                    .unwrap()
                    .missing_message_bytes()
                    .unwrap_or(0)
                    > 0;
                if are_bytes_missing {
                    let available_for_hello = std::cmp::min(
                        buf.len(),
                        self.incoming_message
                            .as_ref()
                            .unwrap()
                            .missing_message_bytes()
                            .unwrap(),
                    );
                    self.incoming_message
                        .as_mut()
                        .map(|im| im.add_data(&buf[..available_for_hello]));
                    processed_bytes += available_for_hello;
                }
            }
        } else {
            let available_for_hello = std::cmp::min(buf.len(), HELLO_START);
            if available_for_hello > 0 {
                let mut im = IncomingMessage::new(self.bep_auth_status);
                im.add_data(&buf[..available_for_hello]);
                self.incoming_message = Some(im);
                processed_bytes += available_for_hello;
            }
        }

        Ok(processed_bytes)
    }

    fn populate_header(&mut self, buf: &[u8]) -> Result<usize, String> {
        let mut processed_bytes = 0;
        if let Some(ref mut im) = self.incoming_message.as_mut() {
            assert!(im.total_byte_len() >= 1);
            if im.total_byte_len() < HEADER_START {
                im.add_data(&buf[..1]);
                processed_bytes += 1;
            } else {
                let available_for_header =
                    std::cmp::min(buf.len(), im.missing_header_bytes().unwrap());

                im.add_data(&buf[..available_for_header]);

                processed_bytes += available_for_header;
            }
        } else {
            let available_for_header = std::cmp::min(buf.len(), 2);
            if available_for_header > 0 {
                let mut im = IncomingMessage::new(self.bep_auth_status);
                im.add_data(&buf[..available_for_header]);
                self.incoming_message = Some(im);
                processed_bytes += available_for_header;
            }
        }
        Ok(processed_bytes)
    }
}

// TODO: maybe change name to something like BepConnectionHandler
pub struct BepProcessor {
    connection: OpenConnection,
    receiver: Receiver<BepAction>,
    // Last meaningful message
    last_message_sent_time: Option<Instant>,
    incoming_message: Option<IncomingMessage>,
    // TODO: think if we should have these in a ClientState struct
    local_id: DeviceId,
    index: syncthing::Index,
    client_id: HashSet<DeviceId>,
    data_processor: BepDataProcessor,
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
            incoming_message: None,
            local_id,
            client_id,
            index,
            data_processor: BepDataProcessor::new(),
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
                    send_ping(&mut self.connection);
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
                debug!("plaintext read {:#04x?}", &buf);
                self.data_processor
                    .process_incoming_data(&buf)
                    .map(|cm| self.handle_complete_messages(cm))
            }
            Err(e) => Err(format!("Received event was {}", e)),
        };
        match res {
            Err(e) => warn!("Reading incoming data failed with {}", e),
            _ => {}
        }
    }

    // fn populate_header(&mut self, buf: &[u8]) -> Result<usize, String> {
    //     let mut processed_bytes = 0;
    //     if let Some(ref mut im) = self.incoming_message.as_mut() {
    //         assert!(im.total_byte_len() >= 1);
    //         if im.total_byte_len() < HEADER_START {
    //             im.add_data(&buf[..1]);
    //             processed_bytes += 1;
    //         } else {
    //             let available_for_header =
    //                 std::cmp::min(buf.len(), im.missing_header_bytes().unwrap());

    //             im.add_data(&buf[..available_for_header]);

    //             processed_bytes += available_for_header;
    //         }
    //     } else {
    //         let available_for_header = std::cmp::min(buf.len(), 2);
    //         if available_for_header > 0 {
    //             let mut im = IncomingMessage::new(self.bep_auth_status);
    //             im.add_data(&buf[..available_for_header]);
    //             self.incoming_message = Some(im);
    //             processed_bytes += available_for_header;
    //         }
    //     }
    //     Ok(processed_bytes)
    // }

    // Returns the amount of processed bytes.
    // fn handle_post_hello_data(&mut self, buf: &[u8]) -> Result<usize, String> {
    //     debug!("read buf len: {}", &buf.len());
    //     trace!("plaintext read {:#04x?}", &buf);

    //     let mut processed_bytes: usize = 0;

    //     if let Some(ref mut im) = self.incoming_message.as_mut() {
    //         debug!("Extending message by {}", buf.len());
    //         // FIXME: this is wrong, this might add too much data
    //         if im.missing_bytes() <= buf.len() {
    //             im.add_data(&buf[..im.missing_bytes()]);
    //         }
    //     } else {
    //         self.incoming_message =
    //             extract_header(&buf)
    //                 .ok()
    //                 .map(|(header, message_byte_size, message_pos_start)| {
    //                     let message_pos_end = message_pos_start + message_byte_size;
    //                     let message_pos_end = std::cmp::min(message_pos_end, buf.len());

    //                     IncomingMessage {
    //                         message_byte_size,
    //                         message_type: syncthing::MessageType::from_i32(header.r#type).unwrap(),
    //                         received_message: Vec::from(&buf[message_pos_start..message_pos_end]),
    //                         header,
    //                     }
    //                 });
    //     }

    //     // FIXME: report the errors that can occur when extracting the headers
    //     if let Some(im) = &self.incoming_message {
    //         if im.missing_bytes() > 0 {
    //             debug!("Missing bytes {:?}", im.missing_bytes());
    //         } else {
    //             debug!("Received whole message {:?}", im.message_type);
    //             // TODO: it should be possible to move the header as it will be removed
    //             // later.
    //             self.decode_message(im.header.clone(), &im.received_message.clone());
    //             self.incoming_message = None;
    //         }
    //     }
    //     todo!();
    // }

    fn handle_complete_messages(
        &self,
        complete_messages: Vec<CompleteMessage>,
    ) -> Result<(), String> {
        todo!();
    }

    fn handle_hello(&mut self, buf: &[u8]) -> Result<usize, String> {
        let message_byte_size: usize = u16::from_be_bytes(buf[4..6].try_into().unwrap()).into();
        let hello = syncthing::Hello::decode(&buf[6..6 + message_byte_size]).unwrap();

        debug!("Received {:?}", hello);

        self.send_hello();
        self.send_cluster_config();
        self.send_index();

        Ok(6 + message_byte_size)
    }

    fn send_hello(&mut self) {
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
        self.connection.write_all(&message).unwrap();
    }

    fn send_cluster_config(&mut self) {
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
        send_message(header, cluster_config, &mut self.connection);
    }

    fn send_index(&mut self) {
        let header = syncthing::Header {
            compression: 0,
            r#type: syncthing::MessageType::Index.into(),
        };

        debug!("Sending Index");
        debug!("Sending Index: {:?}", &self.index);
        send_message(header, self.index.clone(), &mut self.connection);
    }

    // FIXME: don't use index this way, read it from self or something
    fn send_request(&mut self, index: syncthing::Index) {
        let header = syncthing::Header {
            compression: 0,
            r#type: syncthing::MessageType::Request.into(),
        };

        if index.folder.starts_with("test_") {
            return;
        }

        for file in index.files.iter() {
            if file.name != "EBPLEATKTYXKCPEASMCJ" {
                continue;
            }
            for block in file.blocks.iter() {
                let id = rand::random::<i32>().abs();
                let request = syncthing::Request {
                    id,
                    folder: index.folder.clone(),
                    name: file.name.clone(),
                    offset: block.offset,
                    size: block.size,
                    hash: block.hash.clone(),
                    from_temporary: false,
                };
                debug!("Sending Request");
                trace!("Sending Request {:?}", request);
                send_message(header.clone(), request, &mut self.connection);
            }
        }
    }

    fn send_response(&mut self, request: &syncthing::Request) -> Result<(), String> {
        Ok(())
        // let header = syncthing::Header {
        //     compression: 0,
        //     r#type: syncthing::MessageType::Response.into(),
        // };

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
        // send_message(header, response, &mut self.connection)
    }

    fn decode_message(&mut self, header: syncthing::Header, buf: &[u8]) {
        if buf.len() == 0 {
            return;
        }

        let decompressed_raw_message =
            match syncthing::MessageCompression::from_i32(header.compression) {
                Some(syncthing::MessageCompression::Lz4) => {
                    let decompress_byte_size_start = 0;
                    let decompress_byte_size_end = decompress_byte_size_start + 4;
                    let decompressed_byte_size: usize = u32::from_be_bytes(
                        buf[decompress_byte_size_start..decompress_byte_size_end]
                            .try_into()
                            .unwrap(),
                    )
                    .try_into()
                    .unwrap();
                    let compressed_message_start = decompress_byte_size_end;
                    Some(
                        lz4_flex::decompress(
                            &buf[compressed_message_start..],
                            decompressed_byte_size,
                        )
                        .unwrap(),
                    )
                }
                _ => None,
            };

        let raw_message = match decompressed_raw_message.as_ref() {
            Some(x) => x,
            None => &buf[..],
        };

        match syncthing::MessageType::from_i32(header.r#type).unwrap() {
            syncthing::MessageType::ClusterConfig => handle_cluster_config(raw_message),
            syncthing::MessageType::Index => self.handle_index(raw_message),
            syncthing::MessageType::IndexUpdate => self.handle_index(raw_message),
            syncthing::MessageType::Request => self.handle_request(raw_message),
            syncthing::MessageType::Response => handle_response(raw_message),
            syncthing::MessageType::DownloadProgress => self.handle_download_progress(raw_message),
            syncthing::MessageType::Ping => handle_ping(raw_message),
            syncthing::MessageType::Close => handle_close(raw_message),
        }
    }

    fn handle_index(&mut self, buf: &[u8]) {
        debug!("Received Index");

        match syncthing::Index::decode(buf) {
            Ok(index) => {
                debug!("{:?}", index);
                self.send_request(index)
            }
            Err(e) => {
                error!("Error while decoding {:?}", e);
            }
        }
    }

    fn handle_request(&mut self, buf: &[u8]) {
        debug!("Received Request");

        match syncthing::Request::decode(buf) {
            Ok(request) => {
                debug!("{:?}", request);
                self.send_response(&request);
            }
            Err(e) => {
                error!("Error while decoding {:?}", e);
            }
        }
    }

    fn handle_download_progress(&self, raw_message: &[u8]) {
        debug!("Received DownloadProgress");

        match syncthing::DownloadProgress::decode(raw_message) {
            Ok(download_progress) => {
                debug!("{:?}", download_progress);
            }
            Err(e) => {
                error!("Error while decoding {:?}", e);
            }
        }
    }
}

fn try_extract_header(buf: &[u8]) -> Result<syncthing::Header, String> {
    if buf.len() < 2 {
        return Err(format!("Empty Header"));
    }

    // Length of the Header in bytes
    let header_byte_len: usize = u16::from_be_bytes(buf[0..2].try_into().unwrap()).into();

    if header_byte_len == 0 {
        // debug!("Received empty message");
        return Err(format!("Empty Message"));
    }

    trace!("Received Message: {:#04x?}", &buf);

    let header_start = 2;
    let header_end = header_start + header_byte_len;
    let header = syncthing::Header::decode(&buf[header_start..header_end]).unwrap();
    debug!("Received Header: {:?}", &header);
    Ok(header)

    // let message_byte_size_pos_start = header_end;
    // let message_byte_size_pos_end = message_byte_size_pos_start + 4;
    // let message_byte_size: usize = u32::from_be_bytes(
    //     buf[message_byte_size_pos_start..message_byte_size_pos_end]
    //         .try_into()
    //         .unwrap(),
    // )
    // .try_into()
    // .unwrap();
    // trace!(
    //     "message_len in bytes: {:#04x?}",
    //     &buf[message_byte_size_pos_start..message_byte_size_pos_end]
    // );
    // debug!("Message byte size: {}", message_byte_size);

    // let message_pos_start = message_byte_size_pos_end;

    // let res = Ok((header, message_byte_size, message_pos_start));
    // debug!("{:?}", res);
    // res
}

// TODO: evaluate if this should be a trait
fn send_message<T: prost::Message>(
    header: syncthing::Header,
    mut raw_message: T,
    connection: &mut OpenConnection,
) -> Result<(), String> {
    let header_bytes: Vec<u8> = header.encode_to_vec();
    let header_len: u16 = header_bytes.len().try_into().unwrap();

    let message_bytes = raw_message.encode_to_vec();
    let message_len: u32 = message_bytes.len().try_into().unwrap();

    // debug!(
    //     "Sending message with header len: {:?}, {:#04x?}",
    //     header_len,
    //     header_len.to_be_bytes().into_iter().collect::<Vec<u8>>()
    // );

    let message: Vec<u8> = vec![]
        .into_iter()
        .chain(header_len.to_be_bytes().into_iter())
        .chain(header_bytes.into_iter())
        .chain(message_len.to_be_bytes().into_iter())
        .chain(message_bytes.into_iter())
        .collect();

    debug!(
        // "Outgoing message: {:#04x?}",
        "Outgoing message: {:02x?}",
        &message.clone().into_iter().collect::<Vec<u8>>()
    );

    connection.write_all(&message).map_err(|e| e.to_string())
}

fn send_ping(connection: &mut OpenConnection) {
    let mut header = syncthing::Header::default();
    header.r#type = syncthing::MessageType::Ping.into();
    header.compression = 0;
    let header_bytes: Vec<u8> = header.encode_to_vec();
    let header_len: u16 = header_bytes.len().try_into().unwrap();

    let ping = syncthing::Ping::default();
    let ping_bytes = ping.encode_to_vec();
    let ping_len: u32 = ping_bytes.len().try_into().unwrap();

    let message: Vec<u8> = vec![]
        .into_iter()
        .chain(header_len.to_be_bytes().into_iter())
        .chain(header_bytes.into_iter())
        .chain(ping_len.to_be_bytes().into_iter())
        .chain(ping_bytes.into_iter())
        .collect();

    debug!("Outgoing ping message: {:?}", &message);

    connection.write_all(&message).unwrap();
}

fn starts_with_magic_number(buf: &[u8]) -> bool {
    buf.len() >= HELLO_LEN_START && buf[..HELLO_LEN_START] == MAGIC_NUMBER
}

fn handle_cluster_config(buf: &[u8]) {
    debug!("Received Cluster Config");

    match syncthing::ClusterConfig::decode(buf) {
        Ok(cluster_config) => {
            debug!("{:#?}", cluster_config)
        }
        Err(e) => {
            error!("Error while decoding {:?}", e)
        }
    }
}

fn handle_response(buf: &[u8]) {
    debug!("Received Response");

    match syncthing::Response::decode(buf) {
        Ok(response) => {
            debug!("{:?}", response)
        }
        Err(e) => {
            error!("Error while decoding {:?}", e)
        }
    }
}

fn handle_ping(buf: &[u8]) {
    debug!("Received Ping len");
    syncthing::Ping::decode(buf).unwrap();
}

fn handle_close(buf: &[u8]) {
    debug!("Received Close");
    let close_message = syncthing::Close::decode(buf).unwrap();
    debug!("{:?}", close_message);
}

#[cfg(test)]
mod tests {
    use crate::core::{BepDataProcessor, CompleteMessage};
    use crate::syncthing;
    use crate::DeviceId;
    use std::io::Write;

    fn setup_logging() {
        env_logger::builder()
            .format(|buf, record| {
                let ts = buf.timestamp_micros();
                writeln!(
                    buf,
                    "{} {} {:?} {}:{}: {}",
                    ts,
                    buf.default_level_style(record.level())
                        .value(record.level()),
                    std::thread::current().id(),
                    record.file().unwrap_or("unknown"),
                    record.line().unwrap_or(0),
                    record.args()
                )
            })
            .init();
    }

    #[rustfmt::skip]
    fn raw_hello_message() -> Vec<u8> {
        vec![
            // Magic
            0x2e, 0xa7, 0xd9, 0x0b,
            // Length
            0x00, 0x37,
            // Hello
            0x0a, 0x0a, 0x72, 0x65, 0x6d, 0x6f, 0x74, 0x65,
            0x2d, 0x64, 0x65, 0x76, 0x12, 0x09, 0x73, 0x79,
            0x6e, 0x63, 0x74, 0x68, 0x69, 0x6e, 0x67, 0x1a,
            0x1e, 0x76, 0x31, 0x2e, 0x32, 0x33, 0x2e, 0x35,
            0x2d, 0x64, 0x65, 0x76, 0x2e, 0x31, 0x37, 0x2e,
            0x67, 0x37, 0x32, 0x32, 0x36, 0x62, 0x38, 0x34,
            0x35, 0x2e, 0x64, 0x69, 0x72, 0x74, 0x79,
        ]
    }

    #[test]
    fn process_incoming_data__hello_single_block__succeeds() {
        let mut bep_data_processor = BepDataProcessor::new();
        let incoming_data: Vec<u8> = raw_hello_message();

        let complete_messages = bep_data_processor.process_incoming_data(&incoming_data);

        assert_eq!(complete_messages.as_ref().unwrap().len(), 1);
        let hello = syncthing::Hello {
            device_name: format!("remote-dev"),
            client_name: format!("syncthing"),
            client_version: format!("v1.23.5-dev.17.g7226b845.dirty"),
        };
        assert_eq!(complete_messages.unwrap()[0], CompleteMessage::Hello(hello));
    }

    #[test]
    fn process_incoming_data__hello_multiple_blocks__succeeds() {
        let mut bep_data_processor = BepDataProcessor::new();
        let incoming_data: Vec<u8> = raw_hello_message();

        let complete_messages = bep_data_processor.process_incoming_data(&incoming_data[..1]);
        let complete_messages = bep_data_processor.process_incoming_data(&incoming_data[1..4]);
        let complete_messages = bep_data_processor.process_incoming_data(&incoming_data[4..10]);
        let complete_messages = bep_data_processor.process_incoming_data(&incoming_data[10..20]);
        let complete_messages = bep_data_processor.process_incoming_data(&incoming_data[20..]);

        assert_eq!(complete_messages.as_ref().unwrap().len(), 1);
        let hello = syncthing::Hello {
            device_name: format!("remote-dev"),
            client_name: format!("syncthing"),
            client_version: format!("v1.23.5-dev.17.g7226b845.dirty"),
        };
        assert_eq!(complete_messages.unwrap()[0], CompleteMessage::Hello(hello));
    }
}
