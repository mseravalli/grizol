#[macro_use]
extern crate log;

use clap::Parser;
use mio::net::{TcpListener, TcpStream};
use rustls::server::{
    AllowAnyAnonymousOrAuthenticatedClient, AllowAnyAuthenticatedClient, NoClientAuth,
};
use rustls::{self, RootCertStore};
use std::collections::HashMap;
use std::fs;
use std::io;
use std::io::{BufReader, Read, Write};
use std::net;
use std::sync::Arc;

// Token for our listening socket.
const LISTENER: mio::Token = mio::Token(0);

// Include the `items` module, which is generated from items.proto.
// It is important to maintain the same structure as in the proto.
pub mod snazzy {
    pub mod items {
        include!(concat!(env!("OUT_DIR"), "/snazzy.items.rs"));
    }
}

use prost::Message;
use snazzy::items;

/// This binds together a TCP listening socket, some outstanding
/// connections, and a TLS server configuration.
struct TlsServer {
    server: TcpListener,
    connections: HashMap<mio::Token, OpenConnection>,
    next_id: usize,
    tls_config: Arc<rustls::ServerConfig>,
}

impl TlsServer {
    fn new(server: TcpListener, cfg: Arc<rustls::ServerConfig>) -> Self {
        Self {
            server,
            connections: HashMap::new(),
            next_id: 2,
            tls_config: cfg,
        }
    }

    fn accept(&mut self, registry: &mio::Registry) -> Result<(), io::Error> {
        loop {
            match self.server.accept() {
                Ok((socket, addr)) => {
                    debug!("Accepting new connection from {:?}", addr);

                    let tls_conn =
                        rustls::ServerConnection::new(Arc::clone(&self.tls_config)).unwrap();

                    let token = mio::Token(self.next_id);
                    self.next_id += 1;

                    let mut connection = OpenConnection::new(socket, token, tls_conn);
                    connection.register(registry);
                    self.connections.insert(token, connection);
                }
                Err(ref err) if err.kind() == io::ErrorKind::WouldBlock => return Ok(()),
                Err(err) => {
                    println!(
                        "encountered error while accepting connection; err={:?}",
                        err
                    );
                    return Err(err);
                }
            }
        }
    }

    fn conn_event(&mut self, registry: &mio::Registry, event: &mio::event::Event) {
        let token = event.token();

        if self.connections.contains_key(&token) {
            self.connections
                .get_mut(&token)
                .unwrap()
                .ready(registry, event);

            if self.connections[&token].is_closed() {
                self.connections.remove(&token);
            }
        }
    }
}

/// This is a connection which has been accepted by the server,
/// and is currently being served.
///
/// It has a TCP-level stream, a TLS-level connection state, and some
/// other state/metadata.
struct OpenConnection {
    socket: TcpStream,
    token: mio::Token,
    closing: bool,
    closed: bool,
    tls_conn: rustls::ServerConnection,
}

// /// Open a plaintext TCP-level connection for forwarded connections.
// fn open_back(mode: &ServerMode) -> Option<TcpStream> {
//     match *mode {
//         ServerMode::Forward(ref port) => {
//             let addr = net::SocketAddrV4::new(net::Ipv4Addr::new(127, 0, 0, 1), *port);
//             let conn = TcpStream::connect(net::SocketAddr::V4(addr)).unwrap();
//             Some(conn)
//         }
//         _ => None,
//     }
// }

// /// This used to be conveniently exposed by mio: map EWOULDBLOCK
// /// errors to something less-errory.
// fn try_read(r: io::Result<usize>) -> io::Result<Option<usize>> {
//     match r {
//         Ok(len) => Ok(Some(len)),
//         Err(e) => {
//             if e.kind() == io::ErrorKind::WouldBlock {
//                 Ok(None)
//             } else {
//                 Err(e)
//             }
//         }
//     }
// }

impl OpenConnection {
    fn new(socket: TcpStream, token: mio::Token, tls_conn: rustls::ServerConnection) -> Self {
        Self {
            socket,
            token,
            closing: false,
            closed: false,
            tls_conn,
        }
    }

    /// We're a connection, and we have something to do.
    fn ready(&mut self, registry: &mio::Registry, ev: &mio::event::Event) {
        // If we're readable: read some TLS.  Then
        // see if that yielded new plaintext.  Then
        // see if the backend is readable too.
        if ev.is_readable() {
            self.do_tls_read();
            self.try_plain_read();
            // self.try_back_read();
        }

        if ev.is_writable() {
            self.do_tls_write_and_handle_error();
        }

        if self.closing {
            let _ = self.socket.shutdown(net::Shutdown::Both);
            // self.close_back();
            self.closed = true;
            self.deregister(registry);
        } else {
            self.reregister(registry);
        }
    }

    // /// Close the backend connection for forwarded sessions.
    // fn close_back(&mut self) {
    //     if self.back.is_some() {
    //         let back = self.back.as_mut().unwrap();
    //         back.shutdown(net::Shutdown::Both).unwrap();
    //     }
    //     self.back = None;
    // }

    fn do_tls_read(&mut self) {
        // Read some TLS data.
        match self.tls_conn.read_tls(&mut self.socket) {
            Err(err) => {
                if let io::ErrorKind::WouldBlock = err.kind() {
                    return;
                }

                error!("read error {:?}", err);
                self.closing = true;
                return;
            }
            Ok(0) => {
                debug!("eof");
                self.closing = true;
                return;
            }
            Ok(_) => {}
        };

        // Process newly-received TLS messages.
        if let Err(err) = self.tls_conn.process_new_packets() {
            error!("cannot process packet: {:?}", err);

            // last gasp write to send any alerts
            self.do_tls_write_and_handle_error();

            self.closing = true;
        }
    }

    fn try_plain_read(&mut self) {
        // Read and process all available plaintext.
        if let Ok(io_state) = self.tls_conn.process_new_packets() {
            if io_state.plaintext_bytes_to_read() > 0 {
                debug!("bytes to read: {}", io_state.plaintext_bytes_to_read());
                let mut buf = Vec::new();
                buf.resize(io_state.plaintext_bytes_to_read(), 0u8);

                self.tls_conn.reader().read_exact(&mut buf).unwrap();

                self.incoming_plaintext(&buf);
            }
        }
    }

    // fn try_back_read(&mut self) {
    //     if self.back.is_none() {
    //         return;
    //     }

    //     // Try a non-blocking read.
    //     let mut buf = [0u8; 1024];
    //     let back = self.back.as_mut().unwrap();
    //     let rc = try_read(back.read(&mut buf));

    //     if rc.is_err() {
    //         error!("backend read failed: {:?}", rc);
    //         self.closing = true;
    //         return;
    //     }

    //     let maybe_len = rc.unwrap();

    //     // If we have a successful but empty read, that's an EOF.
    //     // Otherwise, we shove the data into the TLS session.
    //     match maybe_len {
    //         Some(len) if len == 0 => {
    //             debug!("back eof");
    //             self.closing = true;
    //         }
    //         Some(len) => {
    //             self.tls_conn.writer().write_all(&buf[..len]).unwrap();
    //         }
    //         None => {}
    //     };
    // }

    fn starts_with_magic_number(buf: &[u8]) -> bool {
        buf.len() >= 4 && buf[0..4] == vec![0x2e, 0xa7, 0xd9, 0x0b]
    }

    fn decode_hello(buf: &[u8]) -> items::Hello {
        let message_byte_len: usize = u16::from_be_bytes(buf[4..6].try_into().unwrap()).into();
        items::Hello::decode(&buf[6..6 + message_byte_len]).unwrap()
    }

    fn decode_message(buf: &[u8]) {
        let header_byte_len: usize = u16::from_be_bytes(buf[0..2].try_into().unwrap()).into();

        if header_byte_len > 0 {
            let header_start = 2;
            let header_end = 2 + header_byte_len;
            let header = items::Header::decode(&buf[header_start..header_end]).unwrap();
            debug!("Received Header: {:?}", header);
            match items::MessageType::from_i32(header.r#type).unwrap() {
                items::MessageType::ClusterConfig => {}
                items::MessageType::Index => {}
                items::MessageType::IndexUpdate => {}
                items::MessageType::Request => {}
                items::MessageType::Response => {}
                items::MessageType::DownloadProgress => {}
                items::MessageType::Ping => {}
                items::MessageType::Close => {}
            }
        }

        // let message_byte_len: usize = u16::from_be_bytes(buf[4..6].try_into().unwrap()).into();
        // let message_start = 2 + header_byte_len;
        // let message_end = message_start + message_byte_len;
        // items::Hello::decode(&buf[message_start..message_end]).unwrap()
    }

    fn send_hello(&mut self) {
        let mut hello = items::Hello::default();
        hello.device_name = format!("damorire");
        hello.client_name = format!("mydama");
        hello.client_version = format!("0.0.1");

        trace!("{:#04x?}", &hello.encode_to_vec());

        // TODO: use the right length
        let message: Vec<u8> = vec![0x2e, 0xa7, 0xd9, 0x0b, 0x00, 0x19]
            .into_iter()
            .chain(hello.encode_to_vec().into_iter())
            .collect();

        self.tls_conn.writer().write_all(&message).unwrap();
    }

    /// Process some amount of received plaintext.
    fn incoming_plaintext(&mut self, buf: &[u8]) {
        if Self::starts_with_magic_number(buf) {
            let hello = Self::decode_hello(buf);
            info!("Received {:?}", hello);

            info!("Sending Hello");
            self.send_hello();
        } else {
            trace!("plaintext read {:#04x?}", &buf);
            Self::decode_message(&buf);
        }
    }

    // fn send_http_response_once(&mut self) {
    //     let response =
    //         b"HTTP/1.0 200 OK\r\nConnection: close\r\n\r\nHello world from rustls tlsserver\r\n";
    //     if !self.sent_http_response {
    //         self.tls_conn
    //             .writer()
    //             .write_all(response)
    //             .unwrap();
    //         self.sent_http_response = true;
    //         self.tls_conn.send_close_notify();
    //     }
    // }

    fn tls_write(&mut self) -> io::Result<usize> {
        self.tls_conn.write_tls(&mut self.socket)
    }

    fn do_tls_write_and_handle_error(&mut self) {
        let rc = self.tls_write();
        if rc.is_err() {
            error!("write failed {:?}", rc);
            self.closing = true;
        }
    }

    fn register(&mut self, registry: &mio::Registry) {
        let event_set = self.event_set();
        registry
            .register(&mut self.socket, self.token, event_set)
            .unwrap();

        // if self.back.is_some() {
        //     registry
        //         .register(
        //             self.back.as_mut().unwrap(),
        //             self.token,
        //             mio::Interest::READABLE,
        //         )
        //         .unwrap();
        // }
    }

    fn reregister(&mut self, registry: &mio::Registry) {
        let event_set = self.event_set();
        registry
            .reregister(&mut self.socket, self.token, event_set)
            .unwrap();
    }

    fn deregister(&mut self, registry: &mio::Registry) {
        registry.deregister(&mut self.socket).unwrap();

        // if self.back.is_some() {
        //     registry.deregister(self.back.as_mut().unwrap()).unwrap();
        // }
    }

    /// What IO events we're currently waiting for,
    /// based on wants_read/wants_write.
    fn event_set(&self) -> mio::Interest {
        let rd = self.tls_conn.wants_read();
        let wr = self.tls_conn.wants_write();

        if rd && wr {
            mio::Interest::READABLE | mio::Interest::WRITABLE
        } else if wr {
            mio::Interest::WRITABLE
        } else {
            mio::Interest::READABLE
        }
    }

    fn is_closed(&self) -> bool {
        self.closed
    }
}

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    #[arg(long)]
    key: String,
    #[arg(long)]
    certs: String,
    #[arg(long)]
    port: Option<u16>,
    #[arg(long)]
    protover: Vec<String>,
    #[arg(long)]
    suite: Vec<String>,
    #[arg(long)]
    proto: Vec<String>,
    #[arg(long)]
    ocsp: Option<String>,
    #[arg(long)]
    auth: Option<String>,
    #[arg(long)]
    require_auth: Option<bool>,
    #[arg(long)]
    resumption: Option<bool>,
    #[arg(long)]
    tickets: Option<bool>,
}

fn find_suite(name: &str) -> Option<rustls::SupportedCipherSuite> {
    for suite in rustls::ALL_CIPHER_SUITES {
        let sname = format!("{:?}", suite.suite()).to_lowercase();

        if sname == name.to_string().to_lowercase() {
            return Some(*suite);
        }
    }

    None
}

fn lookup_suites(suites: &[String]) -> Vec<rustls::SupportedCipherSuite> {
    let mut out = Vec::new();

    for csname in suites {
        let scs = find_suite(csname);
        match scs {
            Some(s) => out.push(s),
            None => panic!("cannot look up ciphersuite '{}'", csname),
        }
    }

    out
}

/// Make a vector of protocol versions named in `versions`
fn lookup_versions(versions: &[String]) -> Vec<&'static rustls::SupportedProtocolVersion> {
    let mut out = Vec::new();

    for vname in versions {
        let version = match vname.as_ref() {
            "1.2" => &rustls::version::TLS12,
            "1.3" => &rustls::version::TLS13,
            _ => panic!(
                "cannot look up version '{}', valid are '1.2' and '1.3'",
                vname
            ),
        };
        out.push(version);
    }

    out
}

fn load_certs(filename: &str) -> Vec<rustls::Certificate> {
    let certfile = fs::File::open(filename).expect("cannot open certificate file");
    let mut reader = BufReader::new(certfile);
    rustls_pemfile::certs(&mut reader)
        .unwrap()
        .iter()
        .map(|v| rustls::Certificate(v.clone()))
        .collect()
}

fn load_private_key(filename: &str) -> rustls::PrivateKey {
    let keyfile = fs::File::open(filename).expect("cannot open private key file");
    let mut reader = BufReader::new(keyfile);

    loop {
        match rustls_pemfile::read_one(&mut reader).expect("cannot parse private key .pem file") {
            Some(rustls_pemfile::Item::RSAKey(key)) => return rustls::PrivateKey(key),
            Some(rustls_pemfile::Item::PKCS8Key(key)) => return rustls::PrivateKey(key),
            Some(rustls_pemfile::Item::ECKey(key)) => return rustls::PrivateKey(key),
            None => break,
            _ => {}
        }
    }

    panic!(
        "no keys found in {:?} (encrypted keys not supported)",
        filename
    );
}

fn load_ocsp(filename: &Option<String>) -> Vec<u8> {
    let mut ret = Vec::new();

    if let Some(name) = filename {
        fs::File::open(name)
            .expect("cannot open ocsp file")
            .read_to_end(&mut ret)
            .unwrap();
    }

    ret
}

fn make_config(args: &Args) -> Arc<rustls::ServerConfig> {
    let client_auth = if args.auth.is_some() {
        let roots = load_certs(args.auth.as_ref().unwrap());
        let mut client_auth_roots = RootCertStore::empty();
        for root in roots {
            client_auth_roots.add(&root).unwrap();
        }
        if args.require_auth.unwrap() {
            AllowAnyAuthenticatedClient::new(client_auth_roots).boxed()
        } else {
            AllowAnyAnonymousOrAuthenticatedClient::new(client_auth_roots).boxed()
        }
    } else {
        NoClientAuth::boxed()
    };

    let suites = if !args.suite.is_empty() {
        lookup_suites(&args.suite)
    } else {
        rustls::ALL_CIPHER_SUITES.to_vec()
    };

    let versions = if !args.protover.is_empty() {
        lookup_versions(&args.protover)
    } else {
        rustls::ALL_VERSIONS.to_vec()
    };

    let certs = load_certs(&args.certs);
    let privkey = load_private_key(&args.key);
    let ocsp = load_ocsp(&args.ocsp);

    let mut config = rustls::ServerConfig::builder()
        .with_cipher_suites(&suites)
        .with_safe_default_kx_groups()
        .with_protocol_versions(&versions)
        .expect("inconsistent cipher-suites/versions specified")
        .with_client_cert_verifier(client_auth)
        .with_single_cert_with_ocsp_and_sct(certs, privkey, ocsp, vec![])
        .expect("bad certificates/private key");

    config.key_log = Arc::new(rustls::KeyLogFile::new());

    if args.resumption.unwrap_or(false) {
        config.session_storage = rustls::server::ServerSessionMemoryCache::new(256);
    }

    if args.tickets.unwrap_or(false) {
        config.ticketer = rustls::Ticketer::new().unwrap();
    }

    config.alpn_protocols = args
        .proto
        .iter()
        .map(|proto| proto.as_bytes().to_vec())
        .collect::<Vec<_>>();

    Arc::new(config)
}

fn main() {
    // let version = env!("CARGO_PKG_NAME").to_string() + ", version: " + env!("CARGO_PKG_VERSION");

    let args: Args = Args::parse();

    env_logger::init();

    let mut addr: net::SocketAddr = "0.0.0.0:443".parse().unwrap();
    addr.set_port(args.port.unwrap_or(443));

    let config = make_config(&args);

    let mut listener = TcpListener::bind(addr).expect("cannot listen on port");
    let mut poll = mio::Poll::new().unwrap();
    poll.registry()
        .register(&mut listener, LISTENER, mio::Interest::READABLE)
        .unwrap();

    let mut tlsserv = TlsServer::new(listener, config);
    info!("Created tls server");

    let mut events = mio::Events::with_capacity(256);
    loop {
        poll.poll(&mut events, None).unwrap();

        for event in events.iter() {
            match event.token() {
                LISTENER => {
                    tlsserv
                        .accept(poll.registry())
                        .expect("error accepting socket");
                }
                _ => tlsserv.conn_event(poll.registry(), event),
            }
        }
    }
}
