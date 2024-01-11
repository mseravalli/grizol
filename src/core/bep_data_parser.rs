// Include the `syncthing` module, which is generated from syncthing.proto.
// It is important to maintain the same structure as in the proto.

use crate::syncthing;
use prost::Message;

pub const MAGIC_NUMBER: [u8; 4] = [0x2e, 0xa7, 0xd9, 0x0b];

const HELLO_LEN_START: usize = 4;
const HELLO_START: usize = 6;
const HEADER_START: usize = 2;

#[derive(Debug, Clone, PartialEq, Eq)]
enum ParseError {
    // TODO: explain this, this is used when checking if the message is complete
    NoIncomingMessageYet,
    // Signals that we don't have enough data to parse the header length
    NotEnoughHeaderLenData,
    // Signals that we don't have enough data to parse the header
    NotEnoughHeaderData,
    // The hello message does not start with the magic number
    NoMagicHello,
    Generic(String),
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
#[derive(Default)]
enum IncomingMessageStatus {
    #[default]
    Incomplete,
    Complete,
}



#[derive(Debug)]
struct IncomingMessage {
    auth_status: BepAuthStatus,
    data: Vec<u8>,
    header: Option<syncthing::Header>,
}

impl IncomingMessage {
    fn new(auth_status: BepAuthStatus) -> Self {
        IncomingMessage {
            auth_status,
            data: Default::default(),
            header: Default::default(),
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
                    IncomingMessageStatus::Incomplete
                } else if let Some(0) = self.missing_message_bytes() {
                    IncomingMessageStatus::Complete
                } else {
                    IncomingMessageStatus::Incomplete
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
                        trace!("message byte len: {}", message_byte_len);
                        Some(message_byte_len)
                    } else {
                        None
                    }
                }
            }
        }
    }
    fn add_data(&mut self, buf: &[u8]) {
        self.data.extend_from_slice(buf);

        match self.auth_status {
            BepAuthStatus::PreHello => {}
            BepAuthStatus::PostHello => {
                if self.header.is_none() {
                    match try_parse_header(&self.data) {
                        Ok(header) => self.header = Some(header),
                        Err(ParseError::NotEnoughHeaderData) => {
                            trace!("Not enough data to parse the header yet.");
                        }
                        Err(e) => {
                            debug!("Encountered error when parsing header {:?}", e);
                        }
                    }
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
        

        self
            .header_byte_len()
            .map(|header_byte_len| HEADER_START + header_byte_len - self.total_byte_len())
    }
    fn missing_message_bytes(&self) -> Option<usize> {
        match self.auth_status {
            BepAuthStatus::PreHello => {
                
                self
                    .message_byte_len()
                    .map(|mbl| mbl + HELLO_START - self.data.len())
            }
            BepAuthStatus::PostHello => self.message_start_pos().and_then(|message_start_pos| {
                
                self
                    .message_byte_len()
                    .map(|mbl| mbl + message_start_pos - self.data.len())
            }),
        }
    }
}

#[derive(Debug, PartialEq, Clone)]
pub enum CompleteMessage {
    Hello(syncthing::Hello),
    ClusterConfig(syncthing::ClusterConfig),
    Index(syncthing::Index),
    IndexUpdate(syncthing::IndexUpdate),
    Request(syncthing::Request),
    Response(syncthing::Response),
    DownloadProgress(syncthing::DownloadProgress),
    Ping(syncthing::Ping),
    Close(syncthing::Close),
}

impl TryFrom<&IncomingMessage> for CompleteMessage {
    type Error = ParseError;
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

fn decode_hello(buf: &[u8]) -> Result<CompleteMessage, ParseError> {
    if !starts_with_magic_number(buf) {
        return Err(ParseError::NoMagicHello);
    }
    // FIXME: return the errors
    let message_byte_size: usize =
        u16::from_be_bytes(buf[HELLO_LEN_START..HELLO_START].try_into().unwrap()).into();
    let hello =
        syncthing::Hello::decode(&buf[HELLO_START..HELLO_START + message_byte_size]).unwrap();
    Ok(CompleteMessage::Hello(hello))
}

fn decode_post_hello_message(im: &IncomingMessage) -> Result<CompleteMessage, ParseError> {
    trace!("message size: {:?}", &im.message_byte_len());
    trace!("data.len() {:?}", &im.data.len());
    let message_start_pos = im.message_start_pos().unwrap();
    // TODO: Use a refence here
    let header = im.header.as_ref().unwrap().clone();
    let decompressed_raw_message = match syncthing::MessageCompression::try_from(header.compression)
    {
        Ok(syncthing::MessageCompression::Lz4) => {
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

    let raw_msg = match decompressed_raw_message.as_ref() {
        Some(x) => x,
        None => &im.data[message_start_pos..],
    };

    trace!("Raw message to decode len {}", &raw_msg.len());

    let complete_message: CompleteMessage = match syncthing::MessageType::try_from(header.r#type)
        .unwrap()
    {
        syncthing::MessageType::ClusterConfig => {
            syncthing::ClusterConfig::decode(raw_msg).map(CompleteMessage::ClusterConfig)
        }
        syncthing::MessageType::Index => {
            syncthing::Index::decode(raw_msg).map(CompleteMessage::Index)
        }
        syncthing::MessageType::IndexUpdate => {
            syncthing::IndexUpdate::decode(raw_msg).map(CompleteMessage::IndexUpdate)
        }
        syncthing::MessageType::Request => {
            syncthing::Request::decode(raw_msg).map(CompleteMessage::Request)
        }
        syncthing::MessageType::Response => {
            syncthing::Response::decode(raw_msg).map(CompleteMessage::Response)
        }
        syncthing::MessageType::DownloadProgress => syncthing::DownloadProgress::decode(raw_msg)
            .map(CompleteMessage::DownloadProgress),
        syncthing::MessageType::Ping => {
            syncthing::Ping::decode(raw_msg).map(CompleteMessage::Ping)
        }
        syncthing::MessageType::Close => {
            syncthing::Close::decode(raw_msg).map(CompleteMessage::Close)
        }
    }
    .map_err(|e| ParseError::Generic(format!("Error: {:?}", e)))?;

    trace!("Post auth message: {:?}", complete_message);
    Ok(complete_message)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BepAuthStatus {
    PreHello,
    PostHello,
}

#[derive(Debug)]
pub struct BepDataParser {
    bep_auth_status: BepAuthStatus,
    incoming_message: Option<IncomingMessage>,
}

impl BepDataParser {
    pub fn new() -> Self {
        BepDataParser {
            bep_auth_status: BepAuthStatus::PreHello,
            incoming_message: None,
        }
    }
    pub fn parse_incoming_data(&mut self, buf: &[u8]) -> Result<Vec<CompleteMessage>, String> {
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
                        .and_then(|im| im.header.as_ref())
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
                    let complete_message: Result<CompleteMessage, ParseError> = im.try_into();
                    if self.bep_auth_status == BepAuthStatus::PreHello {
                        self.bep_auth_status = BepAuthStatus::PostHello;
                    }
                    complete_message
                }
                _ => Err(ParseError::NoIncomingMessageYet),
            };

            match complete_message {
                Ok(cm) => {
                    res.push(cm);
                    self.incoming_message = None;
                }
                Err(ParseError::NoIncomingMessageYet) => {
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
                    if let Some(im) = self.incoming_message
                        .as_mut() { im.add_data(&buf[..available_for_hello]) }
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
                Ok(available_bytes)
            } else {
                let header_byte_len: usize =
                    u16::from_be_bytes(im.data[..HEADER_START].try_into().unwrap()).into();
                let message_len_start = HEADER_START + header_byte_len;
                let message_start = message_len_start + 4;
                let missing_message_len_bytes = message_start - im.data.len();
                let available_bytes = std::cmp::min(missing_message_len_bytes, buf.len());
                im.add_data(&buf[..available_bytes]);
                Ok(available_bytes)
            }
        } else {
            todo!()
        }
    }
}
fn try_parse_header(buf: &[u8]) -> Result<syncthing::Header, ParseError> {
    if buf.len() < HEADER_START {
        return Err(ParseError::NotEnoughHeaderLenData);
    }

    // Length of the Header in bytes
    let header_byte_len: usize = u16::from_be_bytes(buf[..HEADER_START].try_into().unwrap()).into();

    let header_end = HEADER_START + header_byte_len;
    if buf.len() < header_end {
        return Err(ParseError::NotEnoughHeaderData);
    }

    let header = syncthing::Header::decode(&buf[HEADER_START..header_end]).unwrap();
    trace!("Received Header: {:?}", &header);
    Ok(header)
}

#[cfg(test)]
mod test {
    use crate::core::bep_data_parser::BepDataParser;
    use crate::core::bep_data_parser::CompleteMessage;
    use crate::syncthing;

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
    fn parse_incoming_data_hello_single_block_succeeds() {
        let mut data_parser = BepDataParser::new();
        let incoming_data: Vec<u8> = raw_hello_message();

        let complete_messages = data_parser.parse_incoming_data(&incoming_data);

        assert_eq!(complete_messages.as_ref().unwrap().len(), 1);
        let hello = syncthing::Hello {
            device_name: "remote-dev".to_string(),
            client_name: "syncthing".to_string(),
            client_version: "v1.23.5-dev.17.g7226b845.dirty".to_string(),
        };
        assert_eq!(complete_messages.unwrap()[0], CompleteMessage::Hello(hello));
    }

    #[test]
    fn parse_incoming_data_hello_multiple_blocks_succeeds() {
        let mut data_parser = BepDataParser::new();
        let incoming_data: Vec<u8> = raw_hello_message();

        let _complete_messages = data_parser.parse_incoming_data(&incoming_data[..1]);
        let _complete_messages = data_parser.parse_incoming_data(&incoming_data[1..4]);
        let _complete_messages = data_parser.parse_incoming_data(&incoming_data[4..10]);
        let _complete_messages = data_parser.parse_incoming_data(&incoming_data[10..20]);
        let complete_messages = data_parser.parse_incoming_data(&incoming_data[20..]);

        assert_eq!(complete_messages.as_ref().unwrap().len(), 1);
        let hello = syncthing::Hello {
            device_name: "remote-dev".to_string(),
            client_name: "syncthing".to_string(),
            client_version: "v1.23.5-dev.17.g7226b845.dirty".to_string(),
        };
        assert_eq!(complete_messages.unwrap()[0], CompleteMessage::Hello(hello));
    }

    #[test]
    fn parseincomingdata_clustersingleblock_succeeds() {
        let mut data_parser = BepDataParser::new();
        let mut incoming_data: Vec<u8> = vec![];
        incoming_data.extend(raw_hello_message());
        incoming_data.extend(raw_cluster_message());

        let complete_messages = data_parser.parse_incoming_data(&incoming_data);

        assert_eq!(complete_messages.as_ref().unwrap().len(), 3);
    }
}
