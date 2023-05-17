// Include the `syncthing` module, which is generated from syncthing.proto.
// It is important to maintain the same structure as in the proto.
pub mod syncthing {
    include!(concat!(env!("OUT_DIR"), "/syncthing.rs"));
}

use crate::connectivity::OpenConnection;
use prost::Message;
use std::io::Write;
use std::sync::mpsc::{Receiver, Sender};
use syncthing::Header;
use syncthing::Hello;

pub struct BepProcessor {}

impl BepProcessor {
    pub fn process(mut connection: OpenConnection, receiver: Receiver<i32>) -> OpenConnection {
        loop {
            let res = receiver.recv().unwrap();
            match connection.simplified_read() {
                Ok(buf) => {
                    process_incoming_message(&buf, &mut connection);
                }
                Err(e) => {
                    trace!("Received event was {}", e);
                }
            }

            if connection.is_closed() {
                // TODO: add more info
                info!("Connection was closed");
                return connection;
            }
        }
    }
}

fn process_incoming_message(buf: &[u8], tls_conn: &mut OpenConnection) {
    if starts_with_magic_number(buf) {
        handle_hello(buf, tls_conn);
    } else {
        trace!("plaintext read {:#04x?}", &buf);
        decode_message(&buf);
    }
}

fn starts_with_magic_number(buf: &[u8]) -> bool {
    buf.len() >= 4 && buf[0..4] == vec![0x2e, 0xa7, 0xd9, 0x0b]
}

fn handle_hello(buf: &[u8], tls_conn: &mut OpenConnection) {
    let message_byte_len: usize = u16::from_be_bytes(buf[4..6].try_into().unwrap()).into();
    let hello = syncthing::Hello::decode(&buf[6..6 + message_byte_len]).unwrap();

    debug!("Received {:?}", hello);

    send_hello(tls_conn);
}

fn send_hello(tls_conn: &mut OpenConnection) {
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

    tls_conn.write_all(&message).unwrap();
}

fn decode_message(buf: &[u8]) {
    if buf.len() < 2 {
        return;
    }

    // Length of the Header in bytes
    let header_byte_len: usize = u16::from_be_bytes(buf[0..2].try_into().unwrap()).into();

    if header_byte_len > 0 {
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
            syncthing::MessageType::ClusterConfig => {}
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

fn handle_ping(buf: &[u8], message_byte_len: usize) {
    debug!("Received Ping len: {}", message_byte_len);
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
