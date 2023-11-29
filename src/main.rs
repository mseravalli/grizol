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
use crate::core::bep_processor::BepProcessor;
use crate::core::{BepConfig, EncodedMessages};
use crate::device_id::DeviceId;
use clap::Parser;
use data_encoding::BASE32;
use futures::future::FutureExt;
use rustls_pemfile::{certs, rsa_private_keys};
use sha2::{Digest, Sha256};
use sqlx::sqlite::SqlitePoolOptions;
use std::collections::HashMap;
use std::collections::HashSet;
use std::fs;
use std::fs::File;
use std::io::{self, BufReader, Read, Write};
use std::net;
use std::net::ToSocketAddrs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread;
use std::thread::JoinHandle;
use std::time::{Duration, Instant, UNIX_EPOCH};
use tokio::io::{copy, sink, split, AsyncBufReadExt, AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
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

fn trusted_peers() -> HashSet<DeviceId> {
    vec![
        DeviceId::try_from("BTTVHUR-CKWU5YX-JMAULFO-5CMEQ36-FZWEWAE-QFXROCM-WKMOJPZ-KEUOWAS")
            .unwrap(),
        DeviceId::try_from("VHLZSPS-XMVASHL-NRDGZAY-VUS576S-H56LQRK-GGNWYIS-NR4OKBG-VHGD2AQ")
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

    // TODO: get all this data from the config I guess
    let bep_config = BepConfig {
        id: device_id,
        name: format!("Grizol Server"),
        trusted_peers: trusted_peers(),
        base_dir: format!("/home/marco/workspace/hic-sunt-leones/syncthing-test"),
        net_address: addr.to_string(),
    };

    // Using max_connections 1 in order not to have locking issues when running transactions.
    // From https://github.com/launchbadge/sqlx/issues/451#issuecomment-649866619 it might make
    // sense to have 1 pool for reading and 1 pool for writing.
    let db_pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite:target/grizol.db")
        .await
        .expect("Not possible to connect to the sqlite database.");

    let bep_processor = Arc::new(BepProcessor::new(bep_config, db_pool));

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
    // TODO: check if 1<<8 makes sense
    let (em_sender, mut em_receiver) = mpsc::channel::<EncodedMessages>(1 << 8);

    let bep_processor_clone = bep_processor.clone();
    let em_sender_clone = em_sender.clone();
    // FIXME: add a handle here and in case it is reached, return the error
    tokio::spawn(async move {
        let bep_processor = bep_processor_clone;
        let em_sender = em_sender_clone;
        // TODO: check if 1<<16 makes sense
        let mut buf = [0u8; 1 << 16];
        loop {
            let n = reader.read(&mut buf[..]).await?;

            if n == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "Unexpectedly reached end of the stream",
                )) as io::Result<()>;
            }
            // TODO: remove unwrap
            let complete_messages = data_parser.parse_incoming_data(&buf[..n]).unwrap();

            let bep_replies: Vec<_> = complete_messages
                .into_iter()
                .map(|cm| bep_processor.handle_complete_message(cm))
                .flatten()
                .collect();

            for bep_reply in bep_replies.into_iter() {
                if let Err(e) = em_sender.send(bep_reply.await).await {
                    error!("Failed to send message due to {:?}", e);
                    return Err(io::Error::new(
                        io::ErrorKind::BrokenPipe,
                        format!("Failed to send message due to {:?}", e),
                    )) as io::Result<()>;
                }
            }
        }
    });

    // TODO: this is too rudimentary, we need to track the last sent message and act upon that.
    // FIXME: add a handle here and in case it is reached, return the error
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(45)).await;

            let ping = bep_processor.ping().await;

            if let Err(e) = em_sender.send(ping).await {
                error!("Failed to send message due to {:?}", e);
                return Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    format!("Failed to send message due to {:?}", e),
                )) as io::Result<()>;
            }
        }
    });

    while let Some(em) = em_receiver.recv().await {
        writer.write_all(em.data()).await?;
        writer.flush().await?;
    }

    Ok(())
}
