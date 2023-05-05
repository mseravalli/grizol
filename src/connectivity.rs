use crate::core::BepProcessor;
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

pub struct ServerConfigArgs {
    pub auth: Option<String>,
    pub certs: String,
    pub key: String,
    pub ocsp: Option<String>,
    pub proto: Vec<String>,
    pub protover: Vec<String>,
    pub require_auth: Option<bool>,
    pub resumption: Option<bool>,
    pub suite: Vec<String>,
    pub tickets: Option<bool>,
}

impl Into<Arc<rustls::ServerConfig>> for ServerConfigArgs {
    fn into(self) -> Arc<rustls::ServerConfig> {
        let client_auth = if self.auth.is_some() {
            let roots = load_certs(self.auth.as_ref().unwrap());
            let mut client_auth_roots = RootCertStore::empty();
            for root in roots {
                client_auth_roots.add(&root).unwrap();
            }
            if self.require_auth.unwrap() {
                AllowAnyAuthenticatedClient::new(client_auth_roots).boxed()
            } else {
                AllowAnyAnonymousOrAuthenticatedClient::new(client_auth_roots).boxed()
            }
        } else {
            NoClientAuth::boxed()
        };

        let suites = if !self.suite.is_empty() {
            lookup_suites(&self.suite)
        } else {
            rustls::ALL_CIPHER_SUITES.to_vec()
        };

        let versions = if !self.protover.is_empty() {
            lookup_versions(&self.protover)
        } else {
            rustls::ALL_VERSIONS.to_vec()
        };

        let certs = load_certs(&self.certs);
        let privkey = load_private_key(&self.key);
        let ocsp = load_ocsp(&self.ocsp);

        let mut config = rustls::ServerConfig::builder()
            .with_cipher_suites(&suites)
            .with_safe_default_kx_groups()
            .with_protocol_versions(&versions)
            .expect("inconsistent cipher-suites/versions specified")
            .with_client_cert_verifier(client_auth)
            .with_single_cert_with_ocsp_and_sct(certs, privkey, ocsp, vec![])
            .expect("bad certificates/private key");

        config.key_log = Arc::new(rustls::KeyLogFile::new());

        if self.resumption.unwrap_or(false) {
            config.session_storage = rustls::server::ServerSessionMemoryCache::new(256);
        }

        if self.tickets.unwrap_or(false) {
            config.ticketer = rustls::Ticketer::new().unwrap();
        }

        config.alpn_protocols = self
            .proto
            .iter()
            .map(|proto| proto.as_bytes().to_vec())
            .collect::<Vec<_>>();

        Arc::new(config)
    }
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

/// This binds together a TCP listening socket, some outstanding
/// connections, and a TLS server configuration.
pub struct TlsServer {
    server: TcpListener,
    connections: HashMap<mio::Token, OpenConnection>,
    next_id: usize,
    tls_config: Arc<rustls::ServerConfig>,
}

impl TlsServer {
    pub fn new(server: TcpListener, cfg: Arc<rustls::ServerConfig>) -> Self {
        Self {
            server,
            connections: HashMap::new(),
            next_id: 2,
            tls_config: cfg,
        }
    }

    pub fn accept(
        &mut self,
        registry: &mio::Registry,
        bep_processor: BepProcessor,
    ) -> Result<(), io::Error> {
        loop {
            match self.server.accept() {
                Ok((socket, addr)) => {
                    debug!("Accepting new connection from {:?}", addr);

                    let tls_conn =
                        rustls::ServerConnection::new(Arc::clone(&self.tls_config)).unwrap();

                    let token = mio::Token(self.next_id);
                    self.next_id += 1;

                    let mut connection =
                        OpenConnection::new(socket, token, tls_conn, bep_processor);
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

    pub fn conn_event(&mut self, registry: &mio::Registry, event: &mio::event::Event) {
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
    bep_processor: BepProcessor,
}

impl OpenConnection {
    fn new(
        socket: TcpStream,
        token: mio::Token,
        tls_conn: rustls::ServerConnection,
        bep_processor: BepProcessor,
    ) -> Self {
        Self {
            socket,
            token,
            closing: false,
            closed: false,
            tls_conn,
            bep_processor,
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
        }

        if ev.is_writable() {
            self.do_tls_write_and_handle_error();
        }

        if self.closing {
            let _ = self.socket.shutdown(net::Shutdown::Both);
            self.closed = true;
            self.deregister(registry);
        } else {
            self.reregister(registry);
        }
    }

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
        match self.tls_conn.process_new_packets() {
            Ok(io_state) => {
                if io_state.plaintext_bytes_to_read() > 0 {
                    debug!("bytes to read: {}", io_state.plaintext_bytes_to_read());
                    let mut buf = Vec::new();
                    buf.resize(io_state.plaintext_bytes_to_read(), 0u8);

                    self.tls_conn.reader().read_exact(&mut buf).unwrap();

                    self.incoming_plaintext(&buf);
                }
            }
            Err(x) => {
                warn!("not possible to read because of {:?}", x);
            }
        }
    }

    /// Process some amount of received plaintext.
    fn incoming_plaintext(&mut self, buf: &[u8]) {
        self.bep_processor.process_incoming_message(buf);
    }

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
    }

    fn reregister(&mut self, registry: &mio::Registry) {
        let event_set = self.event_set();
        registry
            .reregister(&mut self.socket, self.token, event_set)
            .unwrap();
    }

    fn deregister(&mut self, registry: &mio::Registry) {
        registry.deregister(&mut self.socket).unwrap();
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
