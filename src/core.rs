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
    device_id: [u8; 32],
    connection: OpenConnection,
    receiver: Receiver<BepAction>,
    // Last meaningful message
    last_message_sent_time: Option<Instant>,
}

impl BepProcessor {
    pub fn new(
        device_id: [u8; 32],
        connection: OpenConnection,
        receiver: Receiver<BepAction>,
    ) -> Self {
        BepProcessor {
            device_id,
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
        let message_byte_len: usize = u16::from_be_bytes(buf[4..6].try_into().unwrap()).into();
        let hello = syncthing::Hello::decode(&buf[6..6 + message_byte_len]).unwrap();

        debug!("Received {:?}", hello);

        self.send_hello();
        self.send_cluster_config();
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
        let mut header = syncthing::Header::default();
        header.r#type = syncthing::MessageType::ClusterConfig.into();
        header.compression = 0;
        let header_bytes: Vec<u8> = header.encode_to_vec();
        let header_len: u16 = header_bytes.len().try_into().unwrap();

        let this_device = syncthing::Device {
            id: (self.device_id).into(),
            name: format!("damorire"),
            addresses: vec![format!("127.0.0.1:23456")],
            compression: syncthing::Compression::Never.into(),
            max_sequence: 0,
            index_id: 23423534524,
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
            index_id: 2342353323423,
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

        let mut cluster_config = syncthing::ClusterConfig::default();
        cluster_config.folders = vec![folder];
        let cluster_config_bytes = cluster_config.encode_to_vec();
        let cluster_config_len: u32 = cluster_config_bytes.len().try_into().unwrap();

        let message: Vec<u8> = vec![]
            .into_iter()
            .chain(header_len.to_be_bytes().into_iter())
            .chain(header_bytes.into_iter())
            .chain(cluster_config_len.to_be_bytes().into_iter())
            .chain(cluster_config_bytes.into_iter())
            .collect();

        trace!("Outgoing cluster_config message: {:#04x?}", &message);

        self.connection.write_all(&message).unwrap();
    }
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

    if header_byte_len > 0 {
        trace!("Received Message: {:#04x?}", &buf);
        let header_start = 2;
        let header_end = 2 + header_byte_len;
        let header = syncthing::Header::decode(&buf[header_start..header_end]).unwrap();
        debug!("Received Header: {:?}", header);
        let message_start = header_end + 4;
        let message_byte_len: usize =
            u32::from_be_bytes(buf[header_end..message_start].try_into().unwrap())
                .try_into()
                .unwrap();
        match syncthing::MessageType::from_i32(header.r#type).unwrap() {
            syncthing::MessageType::ClusterConfig => {
                handle_cluster_config(&buf[message_start..], message_byte_len);
            }
            syncthing::MessageType::Index => {}
            syncthing::MessageType::IndexUpdate => {}
            syncthing::MessageType::Request => {}
            syncthing::MessageType::Response => {}
            syncthing::MessageType::DownloadProgress => {}
            syncthing::MessageType::Ping => {
                handle_ping(&buf[message_start..], message_byte_len);
            }
            syncthing::MessageType::Close => {
                handle_close(&buf[message_start..], message_byte_len);
            }
        }
    } else {
        debug!("Received empty message");
    }
}
fn handle_cluster_config(buf: &[u8], message_byte_len: usize) {
    debug!("Received Cluster Config");
    if message_byte_len > 0 {
        let decompressed_size: usize = u32::from_be_bytes(buf[..4].try_into().unwrap())
            .try_into()
            .unwrap();
        debug!("decompressed_size {}", decompressed_size);
        let decompressed_cluster_config =
            // FIXME: use this for all messages put it directly in decode message
            lz4_flex::decompress(&buf[4..], decompressed_size).unwrap();

        match syncthing::ClusterConfig::decode(&*decompressed_cluster_config) {
            Ok(cluster_config) => {
                debug!("{:?}", cluster_config)
            }
            Err(e) => {
                error!("Error while decoding {:?}", e)
            }
        }
    }
}

fn handle_ping(buf: &[u8], message_byte_len: usize) {
    debug!("Received Ping len");
    if message_byte_len > 0 {
        syncthing::Ping::decode(&buf[..message_byte_len]).unwrap();
    }
}

fn handle_close(buf: &[u8], message_byte_len: usize) {
    debug!("Received Close");
    if message_byte_len > 0 {
        let close_message = syncthing::Close::decode(&buf[..message_byte_len]).unwrap();
        debug!("{:?}", close_message);
    }
}
