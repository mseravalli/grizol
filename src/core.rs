// Include the `syncthing` module, which is generated from syncthing.proto.
// It is important to maintain the same structure as in the proto.
pub mod syncthing {
    include!(concat!(env!("OUT_DIR"), "/syncthing.rs"));
}
// use crate::syncthing::Header;
// use crate::syncthing::Hello;
use prost::Message;

#[derive(Default, Clone, Copy)]
pub struct BepProcessor {}

impl BepProcessor {
    pub fn process_incoming_message(&self, buf: &[u8]) {
        if starts_with_magic_number(buf) {
            let hello = decode_hello(buf);
            info!("Received {:?}", hello);

            info!("Sending Hello");
            // self.send_hello();
        } else {
            trace!("plaintext read {:#04x?}", &buf);
            decode_message(&buf);
        }
    }
}

fn starts_with_magic_number(buf: &[u8]) -> bool {
    buf.len() >= 4 && buf[0..4] == vec![0x2e, 0xa7, 0xd9, 0x0b]
}

fn decode_hello(buf: &[u8]) -> syncthing::Hello {
    let message_byte_len: usize = u16::from_be_bytes(buf[4..6].try_into().unwrap()).into();
    syncthing::Hello::decode(&buf[6..6 + message_byte_len]).unwrap()
}

fn decode_message(buf: &[u8]) {
    let header_byte_len: usize = u16::from_be_bytes(buf[0..2].try_into().unwrap()).into();

    if header_byte_len > 0 {
        let header_start = 2;
        let header_end = 2 + header_byte_len;
        let header = syncthing::Header::decode(&buf[header_start..header_end]).unwrap();
        debug!("Received Header: {:?}", header);
        match syncthing::MessageType::from_i32(header.r#type).unwrap() {
            syncthing::MessageType::ClusterConfig => {}
            syncthing::MessageType::Index => {}
            syncthing::MessageType::IndexUpdate => {}
            syncthing::MessageType::Request => {}
            syncthing::MessageType::Response => {}
            syncthing::MessageType::DownloadProgress => {}
            syncthing::MessageType::Ping => {}
            syncthing::MessageType::Close => {}
        }
    }

    // let message_byte_len: usize = u16::from_be_bytes(buf[4..6].try_into().unwrap()).into();
    // let message_start = 2 + header_byte_len;
    // let message_end = message_start + message_byte_len;
    // syncthing::Hello::decode(&buf[message_start..message_end]).unwrap()
}

// fn send_hello(&mut self) {
//     let mut hello = syncthing::Hello::default();
//     hello.device_name = format!("damorire");
//     hello.client_name = format!("mydama");
//     hello.client_version = format!("0.0.1");

//     trace!("{:#04x?}", &hello.encode_to_vec());

//     // TODO: use the right length
//     let message: Vec<u8> = vec![0x2e, 0xa7, 0xd9, 0x0b, 0x00, 0x19]
//         .into_iter()
//         .chain(hello.encode_to_vec().into_iter())
//         .collect();

//     self.tls_conn.writer().write_all(&message).unwrap();
// }
