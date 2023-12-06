use crate::device_id::DeviceId;
use crate::grizol;
use clap::Parser;
use data_encoding::BASE32;
use mio::net::{TcpListener, TcpStream};
use rustls::server::{
    AllowAnyAnonymousOrAuthenticatedClient, AllowAnyAuthenticatedClient, ClientCertVerified,
    ClientCertVerifier, NoClientAuth,
};
use rustls::Certificate;
use rustls::DistinguishedName;
use rustls::{self, RootCertStore, ServerConnection};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::collections::HashSet;
use std::fs;
use std::io;
use std::io::{BufReader, Read, Write};
use std::net;
use std::sync::Arc;
use std::time::SystemTime;

const BEP_PROTOCOL_ID: &str = "bep/1.0";

struct PresharedAuth {
    subjects: Vec<DistinguishedName>,
    trusted_peers: HashSet<DeviceId>,
}

impl PresharedAuth {
    #[inline(always)]
    fn boxed(trusted_peers: HashSet<DeviceId>) -> Arc<dyn ClientCertVerifier> {
        // This function is needed because `ClientCertVerifier` is only reachable if the
        // `dangerous_configuration` feature is enabled, which makes coercing hard to outside users
        Arc::new(Self {
            subjects: Default::default(),
            trusted_peers,
        })
    }
}

impl ClientCertVerifier for PresharedAuth {
    fn offer_client_auth(&self) -> bool {
        true
    }

    fn client_auth_root_subjects(&self) -> &[DistinguishedName] {
        &self.subjects
    }

    fn verify_client_cert(
        &self,
        end_entity: &Certificate,
        intermediates: &[Certificate],
        now: SystemTime,
    ) -> Result<ClientCertVerified, rustls::Error> {
        trace!("verifying the certificate");
        let client_id = DeviceId::from(end_entity);

        let any_match = self
            .trusted_peers
            .iter()
            .any(|control_client_id| &client_id == control_client_id);
        if any_match {
            Ok(ClientCertVerified::assertion())
        } else {
            error!("Unknown client device id: {}", client_id.to_string());
            Err(rustls::Error::InvalidCertificate(
                rustls::CertificateError::InvalidPurpose,
            ))
        }
    }
}

impl Into<rustls::ServerConfig> for grizol::Config {
    fn into(self) -> rustls::ServerConfig {
        let trusted_peers = self
            .trusted_peers
            .iter()
            .map(|x| DeviceId::try_from(x.as_str()).unwrap())
            .collect();
        let client_auth = PresharedAuth::boxed(trusted_peers);

        let suites = rustls::ALL_CIPHER_SUITES.to_vec();

        let versions = rustls::ALL_VERSIONS.to_vec();

        let certs = load_certs(&self.cert);
        let privkey = load_private_key(&self.key);

        let mut config = rustls::ServerConfig::builder()
            .with_cipher_suites(&suites)
            .with_safe_default_kx_groups()
            .with_protocol_versions(&versions)
            .expect("inconsistent cipher-suites/versions specified")
            .with_client_cert_verifier(client_auth)
            .with_single_cert_with_ocsp_and_sct(certs, privkey, vec![], vec![])
            .expect("bad certificates/private key");

        config.key_log = Arc::new(rustls::KeyLogFile::new());

        config.alpn_protocols = vec![BEP_PROTOCOL_ID.as_bytes().to_vec()];

        config
    }
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

    pub fn get_connection(&mut self, id: &mio::Token) -> Option<&mut rustls::ServerConnection> {
        debug!("Connection Token: {:?}", &id);
        // TODO: construct
        if self.connections.get(id).is_some() {
            Some(&mut self.connections.get_mut(id).unwrap().tls_conn)
        } else {
            None
        }
    }

    pub fn accept(
        &mut self,
        registry: &mio::Registry,
    ) -> Result<(OpenConnection, mio::Token), io::Error> {
        let mut res = Err(io::Error::new(
            io::ErrorKind::Other,
            "Connection was not created",
        ));
        loop {
            match self.server.accept() {
                Ok((socket, addr)) => {
                    debug!("Accepting new connection from {:?}", addr);

                    let tls_conn = ServerConnection::new(Arc::clone(&self.tls_config)).unwrap();

                    let token = mio::Token(self.next_id);
                    self.next_id += 1;

                    res = Ok((OpenConnection::new(socket, token, tls_conn), token));
                    // let mut connection = OpenConnection::new(socket, token, tls_conn);
                    // connection.register(registry);
                    // self.connections.insert(token, connection);
                    // debug!("Added a new connection");
                }
                // This is triggered when the connection was successfully created.
                Err(ref err) if err.kind() == io::ErrorKind::WouldBlock => {
                    return res;
                }
                Err(err) => {
                    debug!(
                        "encountered error while accepting connection; err={:?}",
                        err
                    );
                    return Err(err);
                }
            }
        }
        res
    }

    // pub fn process_event(
    //     &mut self,
    //     registry: &mio::Registry,
    //     event: &mio::event::Event,
    // ) -> Result<Vec<u8>, String> {
    //     let token = event.token();

    //     if self.connections.contains_key(&token) {
    //         let res = if let Some(connection) = self.connections.get_mut(&token) {
    //             check_client_certs(&connection.tls_conn);
    //             connection.read_event(registry, event)
    //         } else {
    //             Err(format!("Connection not found"))
    //         };

    //         if self.connections[&token].is_closed() {
    //             self.connections.remove(&token);
    //         }
    //         res
    //     } else {
    //         Err(format!("Token {:?} not found", token))
    //     }
    // }
}

/// This is a connection which has been accepted by the server,
/// and is currently being served.
///
/// It has a TCP-level stream, a TLS-level connection state, and some
/// other state/metadata.
#[derive(Debug)]
pub struct OpenConnection {
    socket: TcpStream,
    token: mio::Token,
    closing: bool,
    closed: bool,
    tls_conn: ServerConnection,
}

impl OpenConnection {
    fn new(socket: TcpStream, token: mio::Token, tls_conn: ServerConnection) -> Self {
        Self {
            socket,
            token,
            closing: false,
            closed: false,
            tls_conn,
        }
    }

    pub fn token(&self) -> mio::Token {
        self.token
    }

    pub fn simplified_read(&mut self) -> Result<Vec<u8>, String> {
        debug!("simplified_read");
        self.do_tls_read();
        let res = self.try_plain_read();

        self.do_tls_write_and_handle_error();

        if self.closing {
            let _ = self.socket.shutdown(net::Shutdown::Both);
            self.closed = true;
            // self.deregister(registry);
        } else {
            // self.reregister(registry);
        }

        res
    }

    // /// We're a connection, and we have something to do.
    // fn read_event(
    //     &mut self,
    //     registry: &mio::Registry,
    //     ev: &mio::event::Event,
    // ) -> Result<Vec<u8>, String> {
    //     let res = if ev.is_readable() {
    //         self.do_tls_read();
    //         self.try_plain_read()
    //     } else {
    //         Err(format!("Not readable"))
    //     };

    //     if ev.is_writable() {
    //         self.do_tls_write_and_handle_error();
    //     }

    //     if self.closing {
    //         let _ = self.socket.shutdown(net::Shutdown::Both);
    //         self.closed = true;
    //         self.deregister(registry);
    //     } else {
    //         self.reregister(registry);
    //     }

    //     res
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
            Ok(x) => {
                // debug!("tls read: {:?}", x)
            }
        };

        // Process newly-received TLS messages.
        if let Err(err) = self.tls_conn.process_new_packets() {
            error!("cannot process packet: {:?}", err);

            // last gasp write to send any alerts
            self.do_tls_write_and_handle_error();

            self.closing = true;
        }
    }

    fn try_plain_read(&mut self) -> Result<Vec<u8>, String> {
        // Read and process all available plaintext.
        match self.tls_conn.process_new_packets() {
            Ok(io_state) => {
                trace!("io_state {:?}", io_state);
                if io_state.plaintext_bytes_to_read() > 0 {
                    let mut buf: Vec<u8> = Vec::new();
                    buf.resize(io_state.plaintext_bytes_to_read(), 0);

                    self.tls_conn.reader().read_exact(&mut buf).unwrap();

                    Ok(buf)
                } else {
                    Ok(Vec::new())
                }
            }
            Err(x) => Err(format!("not possible to read because of {:?}", x)),
        }
    }

    // /// Process some amount of received plaintext.
    // fn incoming_plaintext(&mut self, buf: &[u8]) -> Result<&[u8], String> {
    //     Ok(buf)
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

    pub fn register(&mut self, registry: &mio::Registry) {
        registry
            .register(
                &mut self.socket,
                self.token,
                mio::Interest::READABLE | mio::Interest::WRITABLE,
            )
            .unwrap();
    }

    fn reregister(&mut self, registry: &mio::Registry) {
        registry
            .reregister(
                &mut self.socket,
                self.token,
                mio::Interest::READABLE | mio::Interest::WRITABLE,
            )
            .unwrap();
    }

    pub fn deregister(&mut self, registry: &mio::Registry) {
        registry.deregister(&mut self.socket).unwrap();
    }

    pub fn is_closed(&self) -> bool {
        self.closed
    }

    // TODO: check if and we need to flush
    // FIXME: handle errors
    // TODO: https://docs.rs/rustls/latest/rustls/struct.CommonState.html#method.set_buffer_limit
    pub fn write_all(&mut self, message: &[u8]) -> Result<usize, io::Error> {
        // TODO: check if another size makes more sense
        let block = 2 << 14;
        let mut written = 0;

        for i in 0..(message.len() / block) + 1 {
            let to_write = if message.len() > (i + 1) * block {
                block
            } else {
                message.len() - (i * block)
            };

            let write_res = self
                .tls_conn
                .writer()
                .write(&message[i * block..i * block + to_write]);
            trace!("write res {:?}", write_res);
            written += write_res.unwrap_or(0);
            self.tls_write();
        }
        Ok(written)
    }
}
