// Include the `syncthing` module, which is generated from syncthing.proto.
// It is important to maintain the same structure as in the proto.
pub mod syncthing {
    include!(concat!(env!("OUT_DIR"), "/syncthing.rs"));
}

use crate::connectivity::OpenConnection;
use crate::device_id::DeviceId;
use prost::Message;
use std::io::Write;
use std::sync::mpsc::{Receiver, Sender};
use std::time::{Duration, Instant};
use syncthing::Header;
use syncthing::Hello;

const PING_INTERVAL: Duration = Duration::from_secs(45);

pub enum BepAction {
    ReadClientMessage,
}

pub struct BepProcessor {
    local_id: DeviceId,
    connection: OpenConnection,
    receiver: Receiver<BepAction>,
    // Last meaningful message
    last_message_sent_time: Option<Instant>,
}

impl BepProcessor {
    pub fn new(
        local_id: DeviceId,
        connection: OpenConnection,
        receiver: Receiver<BepAction>,
    ) -> Self {
        BepProcessor {
            local_id,
            connection,
            receiver,
            last_message_sent_time: None,
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
                    decode_message(&buf);
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
        let mut hello = syncthing::Hello::default();
        // TODO: use better data here
        hello.device_name = format!("damorire");
        // let version = env!("CARGO_PKG_NAME").to_string() + ", version: " + env!("CARGO_PKG_VERSION");
        hello.client_name = format!("mydama");
        hello.client_version = format!("0.0.1");

        trace!("{:#04x?}", &hello.encode_to_vec());

        // TODO: use the right length
        let message: Vec<u8> = vec![0x2e, 0xa7, 0xd9, 0x0b, 0x00, 0x19]
            .into_iter()
            .chain(hello.encode_to_vec().into_iter())
            .collect();

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
            max_sequence: 0,
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
                "64ZCWTG-22LRVWS-XGRKFV2-OCKORKB-KXPOXIP-ZJRQVS2-5GCIRLH-YWEBFA5",
            )
            .unwrap()
            .into(),
            name: format!("syncthing"),
            addresses: vec![format!("127.0.0.1:220000")],
            compression: syncthing::Compression::Never.into(),
            max_sequence: 0,
            // Delta Index Exchange is not supported yet hence index_id is zero.
            index_id: 0,
            cert_name: String::new(),
            encryption_password_token: vec![],
            introducer: false,
            skip_introduction_removals: true,
            // ..Default::default()
        };

        let folder = syncthing::Folder {
            id: format!("giovanni"),
            label: format!("giovanni"),
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

        let blocks = vec![syncthing::BlockInfo {
            offset: 0,
            size: 100,
            // This is the sha256sum of the block
            hash: vec![
                0xef, 0xd2, 0xc4, 0x6f, 0x61, 0xd3, 0x12, 0xc1, 0xfb, 0x88, 0x0f, 0xfc, 0x05, 0x89,
                0x75, 0x9e, 0xff, 0x34, 0x40, 0x38, 0x64, 0xf9, 0x3d, 0x37, 0x6c, 0x7c, 0x3b, 0x7d,
                0x81, 0x65, 0x28, 0xb5,
            ],
            weak_hash: 0xefd2c46f,
        }];

        let version = Some(syncthing::Vector {
            counters: vec![syncthing::Counter { id: 0, value: 0 }],
        });

        let file_info = syncthing::FileInfo {
            name: format!("test"),
            r#type: syncthing::FileInfoType::File.into(),
            size: 100,
            modified_s: 1685736648,
            modified_by: 1001,
            deleted: false,
            invalid: false,
            no_permissions: true,
            version,
            sequence: 0,
            block_size: 2 << 16,
            blocks,

            ..Default::default() // use the default for the other fields
                                 // uint32       permissions    : ,
                                 // int32        modified_ns    : ,
                                 // string       symlink_target = 17;
        };

        let index = syncthing::Index {
            folder: format!("giovanni"),
            files: vec![file_info],
        };

        debug!("Sending Index");
        send_message(header, index, &mut self.connection);
    }
}

// TODO: evaluate if this should be a trait
fn send_message<T: prost::Message>(
    header: syncthing::Header,
    mut raw_message: T,
    connection: &mut OpenConnection,
) {
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

    connection.write_all(&message).unwrap();
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
    buf.len() >= 4 && buf[0..4] == vec![0x2e, 0xa7, 0xd9, 0x0b]
}

fn decode_message(buf: &[u8]) {
    if buf.len() < 2 {
        return;
    }

    // Length of the Header in bytes
    let header_byte_len: usize = u16::from_be_bytes(buf[0..2].try_into().unwrap()).into();

    if header_byte_len == 0 {
        debug!("Received empty message");
        return;
    }

    trace!("Received Message: {:#04x?}", &buf);
    let header_start = 2;
    let header_end = header_start + header_byte_len;
    let header = syncthing::Header::decode(&buf[header_start..header_end]).unwrap();
    debug!("Received Header: {:?}", header);
    let message_byte_size_start = header_end;
    let message_byte_size_end = message_byte_size_start + 4;
    let message_byte_size: usize = u32::from_be_bytes(
        buf[message_byte_size_start..message_byte_size_end]
            .try_into()
            .unwrap(),
    )
    .try_into()
    .unwrap();

    if message_byte_size == 0 {
        return;
    }

    let message_start = message_byte_size_end;

    let decompressed_raw_message = match syncthing::MessageCompression::from_i32(header.compression)
    {
        Some(syncthing::MessageCompression::Lz4) => {
            let decompress_byte_size_start = message_start;
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
                lz4_flex::decompress(&buf[compressed_message_start..], decompressed_byte_size)
                    .unwrap(),
            )
        }
        _ => None,
    };

    let raw_message = match decompressed_raw_message.as_ref() {
        Some(x) => x,
        None => &buf[message_start..],
    };

    match syncthing::MessageType::from_i32(header.r#type).unwrap() {
        syncthing::MessageType::ClusterConfig => handle_cluster_config(raw_message),
        syncthing::MessageType::Index => handle_index(raw_message),
        syncthing::MessageType::IndexUpdate => handle_index(raw_message),
        syncthing::MessageType::Request => handle_request(raw_message),
        syncthing::MessageType::Response => {}
        syncthing::MessageType::DownloadProgress => {}
        syncthing::MessageType::Ping => handle_ping(raw_message),
        syncthing::MessageType::Close => handle_close(raw_message),
    }
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

fn handle_index(buf: &[u8]) {
    debug!("Received Index");

    match syncthing::Index::decode(buf) {
        Ok(index) => {
            debug!("{:?}", index)
        }
        Err(e) => {
            error!("Error while decoding {:?}", e)
        }
    }
}

fn handle_request(buf: &[u8]) {
    debug!("Received Request");

    match syncthing::Request::decode(buf) {
        Ok(index) => {
            debug!("{:?}", index)
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
