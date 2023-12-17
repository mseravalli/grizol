#[macro_use]
extern crate log;

mod connectivity;
mod core;
mod device_id;
mod storage;

mod syncthing {
    include!(concat!(env!("OUT_DIR"), "/syncthing.rs"));
}
mod grizol {
    include!(concat!(env!("OUT_DIR"), "/grizol.rs"));
}

use crate::connectivity::server_config;
use crate::core::bep_data_parser::{BepDataParser, CompleteMessage};
use crate::core::bep_processor::BepProcessor;
use crate::core::{BepConfig, EncodedMessages};
use crate::device_id::DeviceId;
use clap::Parser;
use data_encoding::BASE32;
use futures::future::FutureExt;
use prost_reflect::{DescriptorPool, DynamicMessage, Value};
use prost_types::FileDescriptorSet;
use rustls_pemfile::{certs, rsa_private_keys};
use sha2::{Digest, Sha256};
use sqlx::sqlite::SqlitePoolOptions;
use std::cell::RefCell;
use std::collections::HashMap;
use std::collections::HashSet;
use std::fs;
use std::fs::File;
use std::io::{self, BufReader, Read, Write};
use std::net;
use std::net::ToSocketAddrs;
use std::path::Path;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::Mutex;
use std::thread;
use std::thread::JoinHandle;
use std::time::{Duration, Instant, UNIX_EPOCH};
use tokio::io::{copy, sink, split, AsyncBufReadExt, AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tokio::task::JoinSet;
use tokio_rustls::rustls::{self, Certificate, PrivateKey};
use tokio_rustls::TlsAcceptor;

#[derive(Parser, Debug, Clone)]
#[command(author, version, about, long_about = None)]
struct Args {
    #[arg(long, short)]
    config: String,
}

fn parse_config(config_path: &str) -> grizol::Config {
    let pool = DescriptorPool::decode(
        include_bytes!(concat!(env!("OUT_DIR"), "/file_descriptor_set_config.bin")).as_ref(),
    )
    .unwrap();
    let message_descriptor = pool.get_message_by_name("grizol.Config").unwrap();

    let config_txt = fs::read_to_string(config_path).expect("cannot open config file");
    let config = DynamicMessage::parse_text_format(message_descriptor, &config_txt).unwrap();
    config.transcode_to().unwrap()
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

#[tokio::main]
async fn main() -> io::Result<()> {
    setup_logging();

    let args: Args = Args::parse();
    let proto_config = parse_config(&args.config);

    let client_device_id: Arc<Mutex<Option<DeviceId>>> = Arc::new(Mutex::new(None));
    let server_config = server_config(proto_config.clone(), client_device_id.clone());
    let acceptor = TlsAcceptor::from(Arc::new(server_config));
    let bep_config = BepConfig::from(proto_config);

    let addr: net::SocketAddr = bep_config.net_address.parse().unwrap();
    let listener = TcpListener::bind(&addr).await?;

    // Using max_connections 1 in order not to have locking issues when running transactions.
    // From https://github.com/launchbadge/sqlx/issues/451#issuecomment-649866619 it might make
    // sense to have 1 pool for reading and 1 pool for writing.
    // TODO: implement multiple connections
    let db_pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect(&bep_config.db_url)
        .await
        .expect("Not possible to connect to the sqlite database.");

    let bep_processor = Arc::new(BepProcessor::new(bep_config, db_pool));

    // We use this to ensure that the device id provided by the connection is assigned only by a
    // sigle client at a time.
    let device_id_assigner: Arc<Mutex<bool>> = Arc::new(Mutex::new(true));

    loop {
        debug!("Wating for a new connection on {}", &addr);

        let (tcp_stream, _peer_addr) = listener.accept().await?;
        let acceptor = acceptor.clone();

        let bep = bep_processor.clone();

        let cdid = client_device_id.clone();
        let dida = device_id_assigner.clone();
        tokio::spawn(async move {
            if let Err(err) = handle_incoming_data(bep, tcp_stream, acceptor, cdid, dida).await {
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
    client_device_id: Arc<Mutex<Option<DeviceId>>>,
    device_id_assigner: Arc<Mutex<bool>>,
) -> io::Result<()> {
    let (tls_stream, cdid) = {
        device_id_assigner.lock().unwrap();
        let tls_stream = acceptor.accept(tcp_stream).await?;
        let cdid: Option<DeviceId> = { client_device_id.lock().unwrap().clone() };
        (tls_stream, cdid.unwrap())
    };
    debug!("Cliend device id {:?}", cdid);

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

            for cm in complete_messages.into_iter() {
                let ems = bep_processor.handle_complete_message(cm, cdid).await;

                for em in ems.into_iter() {
                    if let Err(e) = em_sender.send(em).await {
                        error!("Failed to send message due to {:?}", e);
                        return Err(io::Error::new(
                            io::ErrorKind::BrokenPipe,
                            format!("Failed to send message due to {:?}", e),
                        )) as io::Result<()>;
                    }
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
