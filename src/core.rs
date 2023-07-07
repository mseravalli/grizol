// Include the `syncthing` module, which is generated from syncthing.proto.
// It is important to maintain the same structure as in the proto.
use crate::connectivity::OpenConnection;
use crate::device_id::DeviceId;
use crate::storage;
use crate::storage::index_from_path;
use crate::syncthing;
use prost::Message;
use rand::prelude::*;
use std::io::Write;
use std::path::Path;
use std::sync::mpsc::{Receiver, Sender};
use std::time::{Duration, Instant};
use syncthing::Header;
use syncthing::Hello;

const PING_INTERVAL: Duration = Duration::from_secs(45);
const MAGIC_NUMBER: [u8; 4] = [0x2e, 0xa7, 0xd9, 0x0b];

#[derive(Debug)]
struct ProcessingMessage {
    message_type: syncthing::MessageType,
    message_byte_size: usize,
    received_message: Vec<u8>,
    header: syncthing::Header,
}

impl ProcessingMessage {
    fn missing_bytes(&self) -> usize {
        trace!(
            "type {:?} - message_byte_size {} - received_message.len() {} ",
            self.message_type,
            self.message_byte_size,
            self.received_message.len()
        );
        let missing_bytes = self.message_byte_size - self.received_message.len();
        assert!(missing_bytes >= 0);
        missing_bytes
    }
}

pub enum BepAction {
    ReadClientMessage,
}

pub struct BepProcessor {
    connection: OpenConnection,
    receiver: Receiver<BepAction>,
    // Last meaningful message
    last_message_sent_time: Option<Instant>,
    processing_message: Option<ProcessingMessage>,
    // TODO: think if we should have these in a ClientState struct
    local_id: DeviceId,
    index: syncthing::Index,
}

impl BepProcessor {
    pub fn new(
        local_id: DeviceId,
        connection: OpenConnection,
        receiver: Receiver<BepAction>,
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
            processing_message: None,
            local_id,
            index,
        }
    }
    pub fn run(mut self) -> OpenConnection {
        loop {
            match self.receiver.recv_timeout(Duration::from_millis(100)) {
                Ok(action) => match action {
                    BepAction::ReadClientMessage => {
                        self.read_client_message();
                    }
                },
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
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

    fn read_client_message(&mut self) {
        match self.connection.simplified_read() {
            Ok(buf) => {
                if starts_with_magic_number(&buf) {
                    self.handle_hello(&buf);
                    // TODO: should this go in handle error?
                    self.last_message_sent_time = Some(Instant::now());
                } else {
                    trace!("plaintext read {:#04x?}", &buf);

                    if let Some(ref mut pm) = self.processing_message.as_mut() {
                        debug!("Extending message by {}", buf.len());
                        pm.received_message.extend_from_slice(&buf);
                    } else {
                        self.processing_message = extract_header(&buf).ok().map(
                            |(header, message_byte_size, message_pos_start)| {
                                let message_pos_end = message_pos_start + message_byte_size;
                                let message_pos_end = std::cmp::min(message_pos_end, buf.len());

                                ProcessingMessage {
                                    message_byte_size,
                                    message_type: syncthing::MessageType::from_i32(header.r#type)
                                        .unwrap(),
                                    received_message: Vec::from(
                                        &buf[message_pos_start..message_pos_end],
                                    ),
                                    header,
                                }
                            },
                        );
                    }

                    // FIXME: report the errors that can occur when extracting the headers
                    if let Some(pm) = &self.processing_message {
                        if pm.missing_bytes() > 0 {
                            debug!("Missing bytes {:?}", pm.missing_bytes());
                        } else {
                            debug!("Received whole message {:?}", pm.message_type);
                            // TODO: it should be possible to move the header as it will be removed
                            // later.
                            self.decode_message(pm.header.clone(), &pm.received_message.clone());
                            self.processing_message = None;
                        }
                    }
                }
            }
            Err(e) => {
                trace!("Received event was {}", e);
            }
        }
    }

    fn handle_hello(&mut self, buf: &[u8]) {
        let message_byte_size: usize = u16::from_be_bytes(buf[4..6].try_into().unwrap()).into();
        let hello = syncthing::Hello::decode(&buf[6..6 + message_byte_size]).unwrap();

        debug!("Received {:?}", hello);

        self.send_hello();
        self.send_cluster_config();
        self.send_index();
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

        let client_device = syncthing::Device {
            id: DeviceId::try_from(
                "IBR3S3A-SCETLO7-QRPOGEV-4GBVE4F-CQMV7RI-ZVAMHHT-4VNSLPW-PNV5WAI",
            )
            .unwrap()
            .into(),
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
        let header = syncthing::Header {
            compression: 0,
            r#type: syncthing::MessageType::Response.into(),
        };

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
        // debug!("Sending Response");
        debug!("Sending Response {:?}", response);
        send_message(header, response, &mut self.connection)
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
            syncthing::MessageType::DownloadProgress => {}
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
}

fn extract_header(buf: &[u8]) -> Result<(syncthing::Header, usize, usize), String> {
    if buf.len() < 2 {
        return Err(format!("Empty Header"));
    }

    // Length of the Header in bytes
    let header_byte_len: usize = u16::from_be_bytes(buf[0..2].try_into().unwrap()).into();

    if header_byte_len == 0 {
        debug!("Received empty message");
        return Err(format!("Empty Message"));
    }

    trace!("Received Message: {:#04x?}", &buf);

    let header_start = 2;
    let header_end = header_start + header_byte_len;
    let header = syncthing::Header::decode(&buf[header_start..header_end]).unwrap();
    debug!("Received Header: {:?}", &header);

    let message_byte_size_pos_start = header_end;
    let message_byte_size_pos_end = message_byte_size_pos_start + 4;
    let message_byte_size: usize = u32::from_be_bytes(
        buf[message_byte_size_pos_start..message_byte_size_pos_end]
            .try_into()
            .unwrap(),
    )
    .try_into()
    .unwrap();
    trace!(
        "message_len in bytes: {:#04x?}",
        &buf[message_byte_size_pos_start..message_byte_size_pos_end]
    );

    let message_pos_start = message_byte_size_pos_end;

    let res = Ok((header, message_byte_size, message_pos_start));
    debug!("{:?}", res);
    res
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

    let message: Vec<u8> = vec![]
        .into_iter()
        .chain(header_len.to_be_bytes().into_iter())
        .chain(header_bytes.into_iter())
        .chain(message_len.to_be_bytes().into_iter())
        .chain(message_bytes.into_iter())
        .collect();

    trace!("Outgoing message: {:#04x?}", &message);

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
    buf.len() >= 4 && buf[0..4] == MAGIC_NUMBER
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
            trace!("{:?}", response)
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
