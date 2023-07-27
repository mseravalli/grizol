#[macro_use]
extern crate log;

mod connectivity;
mod core;
mod device_id;
mod storage;

mod syncthing {
    include!(concat!(env!("OUT_DIR"), "/syncthing.rs"));
}

use crate::connectivity::ServerConfigArgs;
use crate::connectivity::{OpenConnection, TlsServer};
use crate::core::bep_data_parser::{BepDataParser, CompleteMessage};
use crate::core::EncodedMessage;
use crate::core::{BepAction, BepProcessor};
use crate::device_id::DeviceId;
use clap::Parser;
use data_encoding::BASE32;
use futures::future::FutureExt;
use rustls_pemfile::{certs, rsa_private_keys};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::collections::HashSet;
use std::fs;
use std::fs::File;
use std::io::{self, BufReader, Read, Write};
use std::net;
use std::net::ToSocketAddrs;
use std::path::{Path, PathBuf};
use std::sync::mpsc::channel;
use std::sync::mpsc::{Receiver, Sender};
use std::sync::Arc;
use std::thread;
use std::thread::JoinHandle;
use std::time::{Duration, Instant, UNIX_EPOCH};
use tokio::io::{copy, sink, split, AsyncBufReadExt, AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::net::TcpStream;
use tokio::task::JoinSet;
use tokio_rustls::rustls::{self, Certificate, PrivateKey};
use tokio_rustls::TlsAcceptor;

// Token for our listening socket.
const LISTENER: mio::Token = mio::Token(0);

#[derive(Parser, Debug, Clone)]
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

impl Into<ServerConfigArgs> for Args {
    fn into(self) -> ServerConfigArgs {
        ServerConfigArgs {
            auth: self.auth,
            certs: self.certs,
            key: self.key,
            ocsp: self.ocsp,
            proto: self.proto,
            protover: self.protover,
            require_auth: self.require_auth,
            resumption: self.resumption,
            suite: self.suite,
            tickets: self.tickets,
            trusted_peers: HashSet::new(),
        }
    }
}

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

struct ClientData {
    sender: Sender<BepAction>,
}

impl ClientData {
    fn new(sender: Sender<BepAction>) -> Self {
        ClientData { sender }
    }
}

fn trusted_peers() -> HashSet<DeviceId> {
    vec![
        DeviceId::try_from("P27XKDE-ZZTZXNS-BDZDV4X-SYIRALM-FDKK5FQ-4IVTONY-URENMYK-EXIFSQ3")
            .unwrap(),
    ]
    .into_iter()
    .collect()
}

fn device_id_from_cert(cert_path: &str) -> DeviceId {
    let certfile = fs::File::open(cert_path).expect("cannot open certificate file");
    let mut reader = BufReader::new(certfile);

    let certs: Vec<rustls::Certificate> = rustls_pemfile::certs(&mut reader)
        .unwrap()
        .iter()
        .map(|v| rustls::Certificate(v.clone()))
        .collect();

    let device_id = DeviceId::from(&certs[0]);

    debug!("My id: {}", device_id.to_string());
    device_id
}

#[tokio::main]
async fn main() -> io::Result<()> {
    setup_logging();

    let args: Args = Args::parse();

    let mut addr: net::SocketAddr = "0.0.0.0:443".parse().unwrap();
    addr.set_port(args.port.unwrap_or(443));
    let device_id = device_id_from_cert(&args.certs);

    let mut server_config_args: ServerConfigArgs = args.into();
    server_config_args.trusted_peers = trusted_peers().clone();
    let config = server_config_args.into();

    let acceptor = TlsAcceptor::from(Arc::new(config));

    let listener = TcpListener::bind(&addr).await?;

    let bep_processor = Arc::new(BepProcessor::new(device_id, trusted_peers()));

    loop {
        debug!("wating for a new connection");

        let (tcp_stream, peer_addr) = listener.accept().await?;
        let acceptor = acceptor.clone();

        let bep = bep_processor.clone();

        tokio::spawn(async move {
            if let Err(err) = handle_incoming_data(bep, tcp_stream, acceptor).await {
                warn!("{:?}", err);
            }
        });
    }

    Ok(())
}

async fn handle_incoming_data(
    bep_processor: Arc<BepProcessor>,
    tcp_stream: TcpStream,
    acceptor: TlsAcceptor,
) -> io::Result<()> {
    let mut tls_stream = acceptor.accept(tcp_stream).await?;

    let (mut reader, mut writer) = split(tls_stream);

    let mut data_parser = BepDataParser::new();
    let mut buf = [0u8; 2 << 16];
    loop {
        let n = reader.read(&mut buf[..]).await?;

        if n == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "Unexpectedly reached end of the stream",
            ));
        }
        // TODO: remove unwrap
        let complete_messages = data_parser.parse_incoming_data(&buf[..n]).unwrap();

        let encoded_messages: Vec<_> = complete_messages
            .into_iter()
            .map(|cm| bep_processor.handle_complete_message(cm))
            .flatten()
            .collect();

        for em in encoded_messages.into_iter() {
            writer.write_all(em.await.data()).await?;
        }
        writer.flush().await?;
    }

    Ok(())
}

