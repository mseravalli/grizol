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

#[derive(Debug, Clone, PartialEq, Eq)]
enum BepError {
    // TODO: explain this, this is used when checking if the message is complete
    NoIncomingMessageYet,
    // Signals that we don't have enough data to parse the header length
    ParseNotEnoughHeaderLenData,
    // Signals that we don't have enough data to parse the message
    ParseZeroLengthHeader,
    // Signals that we don't have enough data to parse the header
    ParseNotEnoughHeaderData,
    // The hello message does not start with the magic number
    NoMagicHello,
    // The incoming message has some data, but the provided len is 0, so the message is empty
    EmptyMessage,
    Generic(String),
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
enum IncomingMessageStatus {
    Incomplete,
    Empty,
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
    // header_byte_len: Option<usize>,
    header: Option<syncthing::Header>,
    // message_byte_len: Option<usize>,
}

impl IncomingMessage {
    fn new(auth_status: BepAuthStatus) -> Self {
        IncomingMessage {
            auth_status,
            data: Default::default(),
            // header_byte_len: Default::default(),
            header: Default::default(),
            // message_byte_len: Default::default(),
        }
    }

    // TODO: structure this a bit better
    fn status(&self) -> IncomingMessageStatus {
        match self.auth_status {
            BepAuthStatus::PreHello => self
                .message_byte_len()
                .map(|mbl| {
                    if self.data.len() == HELLO_START + mbl {
                        IncomingMessageStatus::Complete
                    } else {
                        IncomingMessageStatus::Incomplete
                    }
                })
                .unwrap_or(IncomingMessageStatus::Incomplete),
            BepAuthStatus::PostHello => {
                trace!("status: incoming message: {:02x?}", &self);
                if self.header.is_none() {
                    if let Err(BepError::ParseZeroLengthHeader) = try_parse_header(&self.data) {
                        return IncomingMessageStatus::Empty;
                    }
                    return IncomingMessageStatus::Incomplete;
                } else {
                    if let Some(0) = self.missing_message_bytes() {
                        return IncomingMessageStatus::Complete;
                    } else {
                        return IncomingMessageStatus::Incomplete;
                    }
                }
            }
        }
    }
    fn total_byte_len(&self) -> usize {
        self.data.len()
    }
    fn message_start_pos(&self) -> Option<usize> {
        let header_byte_len: usize =
            u16::from_be_bytes(self.data[..HEADER_START].try_into().unwrap()).into();
        let message_len_start = HEADER_START + header_byte_len;
        let message_start = message_len_start + 4;
        Some(message_start)
    }
    fn message_byte_len(&self) -> Option<usize> {
        match self.auth_status {
            BepAuthStatus::PreHello => {
                if self.data.len() >= HELLO_START {
                    let message_byte_len: usize = u16::from_be_bytes(
                        self.data[HELLO_LEN_START..HELLO_START].try_into().unwrap(),
                    )
                    .into();
                    Some(message_byte_len)
                } else {
                    None
                }
            }
            BepAuthStatus::PostHello => {
                if self.header.is_none() {
                    None
                } else {
                    let header_byte_len: usize =
                        u16::from_be_bytes(self.data[..HEADER_START].try_into().unwrap()).into();
                    let message_len_start = HEADER_START + header_byte_len;
                    // FIXME: use message_start_pos
                    let message_start = message_len_start + 4;
                    if self.data.len() >= message_start {
                        let message_byte_len: usize = u32::from_be_bytes(
                            self.data[message_len_start..message_start]
                                .try_into()
                                .unwrap(),
                        )
                        .try_into()
                        .unwrap();
                        Some(message_byte_len)
                    } else {
                        None
                    }
                }
            }
        }
    }
    fn add_data(&mut self, buf: &[u8]) {
        self.data.extend_from_slice(&buf);

        match self.auth_status {
            BepAuthStatus::PreHello => {
                // if self.data.len() >= HELLO_START && self.message_byte_len.is_none() {
                //     self.message_byte_len = Some(
                //         u16::from_be_bytes(
                //             self.data[HELLO_LEN_START..HELLO_START].try_into().unwrap(),
                //         )
                //         .into(),
                //     );
                // }
            }
            BepAuthStatus::PostHello => {
                if self.header.is_none() {
                    match try_parse_header(&self.data) {
                        Ok(header) => self.header = Some(header),
                        Err(e) => {
                            debug!("Encountered error when parsing header {:?}", e);
                        }
                    }
                } else {
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
            .map(|header_byte_len| HEADER_START + header_byte_len - self.total_byte_len());

        missing_bytes.map(|x| assert!(x >= 0));
        missing_bytes
    }
    fn missing_message_bytes(&self) -> Option<usize> {
        match self.auth_status {
            BepAuthStatus::PreHello => {
                let missing_bytes = self
                    .message_byte_len()
                    .map(|mbl| mbl + HELLO_START - self.data.len());
                missing_bytes.map(|x| assert!(x >= 0));
                missing_bytes
            }
            BepAuthStatus::PostHello => self.message_start_pos().and_then(|message_start_pos| {
                let missing_bytes = self
                    .message_byte_len()
                    .map(|mbl| mbl + message_start_pos - self.data.len());
                missing_bytes.map(|x| assert!(x >= 0));
                missing_bytes
            }),
        }
    }
}

#[derive(Debug, PartialEq)]
enum CompleteMessage {
    Hello(syncthing::Hello),
    ClusterConfig(syncthing::ClusterConfig),
    Index(syncthing::Index),
}

impl TryFrom<&IncomingMessage> for CompleteMessage {
    type Error = BepError;
    fn try_from(input: &IncomingMessage) -> Result<Self, Self::Error> {
        match input.auth_status {
            BepAuthStatus::PreHello => decode_hello(&input.data),
            BepAuthStatus::PostHello => decode_post_hello_message(input),
        }
    }
}

fn starts_with_magic_number(buf: &[u8]) -> bool {
    buf.len() >= HELLO_LEN_START && buf[..HELLO_LEN_START] == MAGIC_NUMBER
}

fn decode_hello(buf: &[u8]) -> Result<CompleteMessage, BepError> {
    if !starts_with_magic_number(&buf) {
        return Err(BepError::NoMagicHello);
    }
    // FIXME: return the errors
    let message_byte_size: usize =
        u16::from_be_bytes(buf[HELLO_LEN_START..HELLO_START].try_into().unwrap()).into();
    let hello =
        syncthing::Hello::decode(&buf[HELLO_START..HELLO_START + message_byte_size]).unwrap();
    Ok(CompleteMessage::Hello(hello))
}

fn decode_post_hello_message(im: &IncomingMessage) -> Result<CompleteMessage, BepError> {
    trace!("message size: {:?}", &im.message_byte_len());
    trace!("data.len() {:?}", &im.data.len());
    let message_start_pos = im.message_start_pos().unwrap();
    // TODO: Use a refence here
    let header = im.header.as_ref().unwrap().clone();
    let decompressed_raw_message = match syncthing::MessageCompression::from_i32(header.compression)
    {
        Some(syncthing::MessageCompression::Lz4) => {
            let decompress_byte_size_start = message_start_pos;
            let decompress_byte_size_end = decompress_byte_size_start + 4;
            let decompressed_byte_size: usize = u32::from_be_bytes(
                im.data[decompress_byte_size_start..decompress_byte_size_end]
                    .try_into()
                    .unwrap(),
            )
            .try_into()
            .unwrap();
            let compressed_message_start = decompress_byte_size_end;
            Some(
                lz4_flex::decompress(&im.data[compressed_message_start..], decompressed_byte_size)
                    .unwrap(),
            )
        }
        _ => None,
    };

    let raw_message = match decompressed_raw_message.as_ref() {
        Some(x) => x,
        None => &im.data[message_start_pos..],
    };

    trace!("Raw message to decode len {}", &raw_message.len());
    // println!("Raw message to decode {:#04x?}", &raw_message);

    let complete_message: CompleteMessage =
        match syncthing::MessageType::from_i32(header.r#type).unwrap() {
            syncthing::MessageType::ClusterConfig => syncthing::ClusterConfig::decode(raw_message)
                .map(|m| CompleteMessage::ClusterConfig(m)),
            syncthing::MessageType::Index => {
                syncthing::Index::decode(raw_message).map(|m| CompleteMessage::Index(m))
            }
            // syncthing::MessageType::IndexUpdate => self.handle_index(raw_message),
            // syncthing::MessageType::Request => self.handle_request(raw_message),
            // syncthing::MessageType::Response => handle_response(raw_message),
            // syncthing::MessageType::DownloadProgress => self.handle_download_progress(raw_message),
            // syncthing::MessageType::Ping => handle_ping(raw_message),
            // syncthing::MessageType::Close => handle_close(raw_message),
            _ => todo!(),
        }
        .map_err(|e| BepError::Generic(format!("Error: {:?}", e)))?;
    println!("Complete message: {:?}", complete_message);
    Ok(complete_message)
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
            trace!(
                "incoming message len {} - processed bytes {}",
                buf.len(),
                processed_bytes
            );
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
                        processed_bytes += self.populate_message(&buf[processed_bytes..])?;
                    }
                }
            };

            let complete_message = match self.incoming_message.as_ref().map(|im| (im, im.status()))
            {
                Some((im, IncomingMessageStatus::Complete)) => {
                    let complete_message: Result<CompleteMessage, BepError> = im.try_into();
                    if self.bep_auth_status == BepAuthStatus::PreHello {
                        self.bep_auth_status = BepAuthStatus::PostHello;
                    }
                    complete_message
                }
                Some((im, IncomingMessageStatus::Empty)) => Err(BepError::EmptyMessage),
                _ => Err(BepError::NoIncomingMessageYet),
            };

            match complete_message {
                Ok(cm) => {
                    res.push(cm);
                    self.incoming_message = None;
                }
                Err(BepError::EmptyMessage) => {
                    self.incoming_message = None;
                }
                Err(BepError::NoIncomingMessageYet) => {
                    trace!("Haven't received enough data to proceed.");
                }
                Err(e) => {
                    return Err(format!(
                        "Got error when dealing with complete message: {:?}",
                        e
                    ));
                }
            };
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

    fn populate_message(&mut self, buf: &[u8]) -> Result<usize, String> {
        // TODO: see if this is a valid assertion
        assert!(self.incoming_message.is_some());
        if let Some(ref mut im) = self.incoming_message.as_mut() {
            if let Some(missing_message_bytes) = im.missing_message_bytes() {
                let available_bytes = std::cmp::min(missing_message_bytes, buf.len());
                im.add_data(&buf[..available_bytes]);
                return Ok(available_bytes);
            } else {
                let header_byte_len: usize =
                    u16::from_be_bytes(im.data[..HEADER_START].try_into().unwrap()).into();
                let message_len_start = HEADER_START + header_byte_len;
                let message_start = message_len_start + 4;
                let missing_message_len_bytes = message_start - im.data.len();
                let available_bytes = std::cmp::min(missing_message_len_bytes, buf.len());
                im.add_data(&buf[..available_bytes]);
                return Ok(available_bytes);
            }
        } else {
            todo!()
        }
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

    fn handle_complete_messages(
        &mut self,
        complete_messages: Vec<CompleteMessage>,
    ) -> Result<(), String> {
        todo!();
        // self.send_hello();
        // self.send_cluster_config();
        // self.send_index();
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

fn try_parse_header(buf: &[u8]) -> Result<syncthing::Header, BepError> {
    if buf.len() < HEADER_START {
        return Err(BepError::ParseNotEnoughHeaderLenData);
    }

    // Length of the Header in bytes
    let header_byte_len: usize = u16::from_be_bytes(buf[..HEADER_START].try_into().unwrap()).into();

    let header_end = HEADER_START + header_byte_len;
    if buf.len() < header_end {
        return Err(BepError::ParseNotEnoughHeaderData);
    }

    let header = syncthing::Header::decode(&buf[HEADER_START..header_end]).unwrap();
    debug!("Received Header: {:?}", &header);
    Ok(header)
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

    fn raw_cluster_message() -> Vec<u8> {
        vec![
            0x00, 0x00, 0x00, 0x00, 0x00, 0xa2, 0x0a, 0x9f, 0x01, 0x0a, 0x06, 0x74, 0x65, 0x73,
            0x74, 0x5f, 0x61, 0x12, 0x06, 0x74, 0x65, 0x73, 0x74, 0x5f, 0x61, 0x82, 0x01, 0x45,
            0x0a, 0x20, 0x18, 0xb6, 0xa9, 0x7b, 0x17, 0xcd, 0x8e, 0x3e, 0x4f, 0x4b, 0xec, 0x31,
            0xe5, 0xa7, 0x46, 0x8f, 0xe6, 0x76, 0xeb, 0x62, 0xbe, 0xf6, 0xb5, 0xb7, 0x96, 0xcf,
            0xca, 0x08, 0x38, 0x15, 0x26, 0xc3, 0x12, 0x06, 0x6d, 0x79, 0x64, 0x61, 0x6d, 0x61,
            0x1a, 0x15, 0x74, 0x63, 0x70, 0x3a, 0x2f, 0x2f, 0x31, 0x32, 0x37, 0x2e, 0x30, 0x2e,
            0x30, 0x2e, 0x31, 0x3a, 0x32, 0x33, 0x34, 0x35, 0x36, 0x20, 0x01, 0x30, 0x0e, 0x82,
            0x01, 0x44, 0x0a, 0x20, 0x7e, 0xbf, 0x75, 0x0c, 0x99, 0xcc, 0xf3, 0x76, 0x84, 0x79,
            0x1d, 0x79, 0x79, 0x61, 0x11, 0x02, 0xca, 0x35, 0x2b, 0xa5, 0x87, 0x11, 0x59, 0xb9,
            0xb4, 0x89, 0x1a, 0xcc, 0x28, 0x97, 0x41, 0x65, 0x12, 0x0a, 0x72, 0x65, 0x6d, 0x6f,
            0x74, 0x65, 0x2d, 0x64, 0x65, 0x76, 0x1a, 0x07, 0x64, 0x79, 0x6e, 0x61, 0x6d, 0x69,
            0x63, 0x30, 0x06, 0x40, 0xf8, 0xad, 0xe3, 0x8d, 0xbb, 0xc8, 0xa6, 0xd3, 0xc4, 0x01,
            0x00, 0x02, 0x08, 0x01, 0x00, 0x00, 0x03, 0x92, 0x0a, 0x06, 0x74, 0x65, 0x73, 0x74,
            0x5f, 0x61, 0x12, 0x94, 0x01, 0x0a, 0x20, 0x35, 0x53, 0x57, 0x52, 0x55, 0x4f, 0x4d,
            0x4f, 0x59, 0x45, 0x44, 0x33, 0x54, 0x4f, 0x35, 0x59, 0x4c, 0x4a, 0x51, 0x51, 0x32,
            0x32, 0x4f, 0x49, 0x43, 0x35, 0x32, 0x4f, 0x33, 0x33, 0x59, 0x52, 0x18, 0x21, 0x20,
            0xa4, 0x83, 0x02, 0x28, 0x94, 0x82, 0x9c, 0xa4, 0x06, 0x4a, 0x0c, 0x0a, 0x0a, 0x08,
            0xbe, 0x9c, 0xb6, 0xbe, 0xb1, 0xaf, 0xaa, 0xdb, 0x18, 0x50, 0x01, 0x58, 0x9b, 0xcb,
            0x9a, 0x82, 0x02, 0x68, 0x80, 0x80, 0x10, 0x72, 0x00, 0x82, 0x01, 0x24, 0x10, 0x21,
            0x1a, 0x20, 0x0b, 0xbd, 0x1f, 0xca, 0x9b, 0xf1, 0xbd, 0x73, 0x4b, 0x75, 0x36, 0xfc,
            0x78, 0xb5, 0xd8, 0x1b, 0xbf, 0x27, 0x4c, 0xc6, 0xcd, 0xb2, 0x94, 0x57, 0x1e, 0xb1,
            0xa8, 0x2a, 0x48, 0xae, 0x88, 0x2c, 0x92, 0x01, 0x20, 0xb6, 0xc3, 0x1a, 0x02, 0xc5,
            0x9e, 0x80, 0x51, 0xa7, 0x27, 0xdb, 0xb7, 0x36, 0x93, 0x85, 0x42, 0x65, 0x4c, 0x1a,
            0x9b, 0x7e, 0x04, 0xca, 0x1c, 0x05, 0x09, 0x18, 0x81, 0xf0, 0x64, 0x2e, 0x59, 0x12,
            0x94, 0x01, 0x0a, 0x20, 0x43, 0x32, 0x34, 0x50, 0x49, 0x4a, 0x46, 0x56, 0x56, 0x48,
            0x55, 0x46, 0x45, 0x55, 0x4f, 0x42, 0x4a, 0x4c, 0x53, 0x4a, 0x41, 0x52, 0x37, 0x35,
            0x34, 0x4b, 0x52, 0x55, 0x32, 0x4e, 0x41, 0x45, 0x18, 0x21, 0x20, 0xa4, 0x83, 0x02,
            0x28, 0x94, 0x82, 0x9c, 0xa4, 0x06, 0x4a, 0x0c, 0x0a, 0x0a, 0x08, 0xbe, 0x9c, 0xb6,
            0xbe, 0xb1, 0xaf, 0xaa, 0xdb, 0x18, 0x50, 0x02, 0x58, 0x9b, 0xcb, 0x9a, 0x82, 0x02,
            0x68, 0x80, 0x80, 0x10, 0x72, 0x00, 0x82, 0x01, 0x24, 0x10, 0x21, 0x1a, 0x20, 0xbc,
            0x4e, 0x1f, 0x8c, 0x0d, 0x2c, 0xfd, 0xe1, 0x72, 0xaa, 0x40, 0x1f, 0xb0, 0x33, 0x50,
            0xc1, 0x8a, 0xf7, 0x4d, 0x1c, 0xf7, 0xc2, 0x77, 0x64, 0x4e, 0xa7, 0x0f, 0x67, 0x65,
            0x7f, 0x2b, 0x01, 0x92, 0x01, 0x20, 0x62, 0x22, 0xe1, 0xe8, 0x3e, 0xc4, 0xd6, 0x94,
            0x3b, 0xce, 0xb9, 0xef, 0x0b, 0x32, 0x12, 0xa2, 0x85, 0xc7, 0x72, 0x58, 0xad, 0xb4,
            0xbf, 0xf3, 0x6a, 0x69, 0x44, 0xf5, 0x8a, 0xf4, 0x3a, 0xdc, 0x12, 0x94, 0x01, 0x0a,
            0x20, 0x52, 0x42, 0x47, 0x33, 0x48, 0x36, 0x36, 0x51, 0x59, 0x53, 0x56, 0x4a, 0x37,
            0x53, 0x42, 0x43, 0x4a, 0x59, 0x51, 0x57, 0x4c, 0x58, 0x49, 0x50, 0x37, 0x49, 0x32,
            0x32, 0x4c, 0x58, 0x46, 0x50, 0x18, 0x21, 0x20, 0xa4, 0x83, 0x02, 0x28, 0x94, 0x82,
            0x9c, 0xa4, 0x06, 0x4a, 0x0c, 0x0a, 0x0a, 0x08, 0xbe, 0x9c, 0xb6, 0xbe, 0xb1, 0xaf,
            0xaa, 0xdb, 0x18, 0x50, 0x03, 0x58, 0x9b, 0xe0, 0x8e, 0x84, 0x02, 0x68, 0x80, 0x80,
            0x10, 0x72, 0x00, 0x82, 0x01, 0x24, 0x10, 0x21, 0x1a, 0x20, 0x35, 0x6f, 0xc5, 0x79,
            0xa7, 0xb1, 0x3c, 0xd7, 0x33, 0x59, 0xfd, 0xe1, 0x12, 0x1d, 0x2b, 0x1b, 0x93, 0x89,
            0x48, 0xbe, 0xee, 0x53, 0x24, 0xe8, 0xea, 0x1a, 0xf5, 0x98, 0x6d, 0x85, 0x27, 0x09,
            0x92, 0x01, 0x20, 0x3d, 0xbf, 0x61, 0xcb, 0xb1, 0xcd, 0xe0, 0x1e, 0x26, 0x7e, 0x00,
            0xc3, 0x1f, 0x48, 0x0b, 0x72, 0xe8, 0x43, 0x43, 0xc2, 0xfd, 0x9c, 0x53, 0x5e, 0xbe,
            0xef, 0x2f, 0x07, 0xff, 0x70, 0xa6, 0x66, 0x12, 0x94, 0x01, 0x0a, 0x20, 0x42, 0x4e,
            0x53, 0x4d, 0x44, 0x35, 0x4c, 0x34, 0x51, 0x45, 0x43, 0x4f, 0x45, 0x34, 0x4c, 0x51,
            0x48, 0x34, 0x50, 0x59, 0x4e, 0x48, 0x49, 0x33, 0x37, 0x52, 0x58, 0x42, 0x49, 0x49,
            0x4a, 0x57, 0x18, 0x21, 0x20, 0xa4, 0x83, 0x02, 0x28, 0x94, 0x82, 0x9c, 0xa4, 0x06,
            0x4a, 0x0c, 0x0a, 0x0a, 0x08, 0xbe, 0x9c, 0xb6, 0xbe, 0xb1, 0xaf, 0xaa, 0xdb, 0x18,
            0x50, 0x04, 0x58, 0x9b, 0xf5, 0x82, 0x86, 0x02, 0x68, 0x80, 0x80, 0x10, 0x72, 0x00,
            0x82, 0x01, 0x24, 0x10, 0x21, 0x1a, 0x20, 0x0c, 0xa4, 0xbd, 0xfe, 0x67, 0x24, 0xe2,
            0x46, 0x0a, 0x9c, 0xbd, 0xde, 0x0c, 0x36, 0x1f, 0x12, 0x2d, 0xc6, 0x87, 0x04, 0xfa,
            0xf7, 0xf4, 0x9e, 0x46, 0xb0, 0xfd, 0x82, 0x4d, 0xc8, 0x9d, 0xb1, 0x92, 0x01, 0x20,
            0x27, 0xca, 0x38, 0xcc, 0x45, 0xed, 0x42, 0x85, 0x5a, 0x70, 0x46, 0x00, 0xcb, 0x1c,
            0x72, 0x95, 0xb1, 0x8c, 0x02, 0x80, 0x33, 0x16, 0x4f, 0x8a, 0xeb, 0xe1, 0xe5, 0x14,
            0xfd, 0xc4, 0xfa, 0x66, 0x12, 0x94, 0x01, 0x0a, 0x20, 0x57, 0x36, 0x55, 0x57, 0x41,
            0x46, 0x4c, 0x45, 0x37, 0x52, 0x43, 0x53, 0x57, 0x34, 0x59, 0x4a, 0x47, 0x52, 0x56,
            0x48, 0x41, 0x48, 0x32, 0x37, 0x35, 0x4c, 0x4f, 0x52, 0x34, 0x4d, 0x33, 0x49, 0x18,
            0x21, 0x20, 0xa4, 0x83, 0x02, 0x28, 0x94, 0x82, 0x9c, 0xa4, 0x06, 0x4a, 0x0c, 0x0a,
            0x0a, 0x08, 0xbe, 0x9c, 0xb6, 0xbe, 0xb1, 0xaf, 0xaa, 0xdb, 0x18, 0x50, 0x05, 0x58,
            0x9b, 0xe0, 0x8e, 0x84, 0x02, 0x68, 0x80, 0x80, 0x10, 0x72, 0x00, 0x82, 0x01, 0x24,
            0x10, 0x21, 0x1a, 0x20, 0xc2, 0xed, 0x34, 0x2a, 0x14, 0x5a, 0x50, 0x67, 0x2f, 0xd3,
            0x74, 0xec, 0x71, 0x4b, 0xf1, 0x26, 0xe9, 0xee, 0xf9, 0x38, 0x18, 0x75, 0x4d, 0x1f,
            0x3d, 0x32, 0xe5, 0x60, 0xed, 0x36, 0xa7, 0x01, 0x92, 0x01, 0x20, 0x5f, 0x9a, 0x92,
            0xdf, 0x04, 0x88, 0x2e, 0x30, 0xf2, 0x2c, 0x9f, 0xc2, 0xee, 0x37, 0x63, 0xe7, 0x08,
            0x95, 0x99, 0x3d, 0x2c, 0x1c, 0xdd, 0xb6, 0xf4, 0x32, 0xbc, 0x47, 0x68, 0xaa, 0x40,
            0x78, 0x12, 0x94, 0x01, 0x0a, 0x20, 0x4c, 0x48, 0x5a, 0x42, 0x58, 0x4e, 0x36, 0x4b,
            0x4f, 0x34, 0x56, 0x49, 0x42, 0x44, 0x57, 0x4d, 0x4c, 0x58, 0x44, 0x55, 0x48, 0x52,
            0x5a, 0x4d, 0x56, 0x54, 0x34, 0x56, 0x47, 0x54, 0x59, 0x54, 0x18, 0x21, 0x20, 0xa4,
            0x83, 0x02, 0x28, 0x94, 0x82, 0x9c, 0xa4, 0x06, 0x4a, 0x0c, 0x0a, 0x0a, 0x08, 0xbe,
            0x9c, 0xb6, 0xbe, 0xb1, 0xaf, 0xaa, 0xdb, 0x18, 0x50, 0x06, 0x58, 0x9a, 0xb6, 0xa6,
            0x80, 0x02, 0x68, 0x80, 0x80, 0x10, 0x72, 0x00, 0x82, 0x01, 0x24, 0x10, 0x21, 0x1a,
            0x20, 0xb0, 0x4d, 0xce, 0x96, 0xda, 0xe1, 0xcb, 0x55, 0x7c, 0x11, 0x06, 0xbe, 0x9d,
            0x08, 0xd0, 0xf4, 0x66, 0x89, 0xdb, 0x77, 0x99, 0xa4, 0x93, 0x1d, 0x84, 0x4b, 0x7d,
            0x30, 0x71, 0x62, 0xaa, 0x44, 0x92, 0x01, 0x20, 0xac, 0xff, 0x75, 0x24, 0xa6, 0xe2,
            0x1f, 0xcd, 0xc0, 0x9f, 0xe5, 0x00, 0x33, 0x84, 0x27, 0x17, 0x14, 0x83, 0xa1, 0x64,
            0xeb, 0x61, 0x7d, 0xf1, 0x80, 0x7f, 0x4c, 0x71, 0xe9, 0x5b, 0x17, 0x56,
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

    #[test]
    fn process_incoming_data__cluster_single_block__succeeds() {
        let mut bep_data_processor = BepDataProcessor::new();
        let mut incoming_data: Vec<u8> = vec![];
        incoming_data.extend(raw_hello_message());
        incoming_data.extend(raw_cluster_message());

        let complete_messages = bep_data_processor.process_incoming_data(&incoming_data);

        assert_eq!(complete_messages.as_ref().unwrap().len(), 3);
    }
}
