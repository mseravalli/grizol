#[macro_use]
extern crate log;

mod connectivity;
mod core;
mod device_id;
mod fuse;
mod storage;

mod syncthing {
    include!(concat!(env!("OUT_DIR"), "/syncthing.rs"));
}
mod grizol {
    include!(concat!(env!("OUT_DIR"), "/grizol.rs"));
}

use crate::connectivity::server_config;
use crate::core::bep_data_parser::BepDataParser;
use crate::core::bep_processor::BepProcessor;
use crate::core::bep_state::BepState;
use crate::core::{EncodedMessages, GrizolConfig};
use crate::device_id::DeviceId;
use crate::fuse::GrizolFS;
use chrono_timesource::UtcTimeSource;
use clap::Parser;
use prost_reflect::{DescriptorPool, DynamicMessage};
use sqlx::sqlite::SqlitePoolOptions;
use sqlx::Executor;
use std::fs;
use std::io::{self, Write};
use std::net;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;
use tokio::io::{split, AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tokio_rustls::TlsAcceptor;

const PING_INTERVAL: Duration = Duration::from_secs(45);

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
    let grizol_config = GrizolConfig::from(proto_config);

    let addr: net::SocketAddr = grizol_config.net_address.parse().unwrap();
    let listener = TcpListener::bind(&addr).await?;

    // Using max_connections 1 in order not to have locking issues when running transactions.
    // From https://github.com/launchbadge/sqlx/issues/451#issuecomment-649866619 it might make
    // sense to have 1 pool for reading and 1 pool for writing.
    // TODO: implement multiple connections
    let db_pool = SqlitePoolOptions::new()
        .max_connections(1)
        .after_connect(|conn, _meta| {
            Box::pin(async move {
                // When directly invoking `Executor` methods,
                // it is possible to execute multiple statements with one call.
                conn.execute("PRAGMA foreign_keys = ON;").await?;

                Ok(())
            })
        })
        .connect(&grizol_config.db_url)
        .await
        .expect("Not possible to connect to the sqlite database.");

    let clock = Arc::new(tokio::sync::Mutex::new(UtcTimeSource {}));

    let bep_state = Arc::new(tokio::sync::Mutex::new(BepState::new(db_pool, clock)));

    let bep_processor = Arc::new(BepProcessor::new(grizol_config.clone(), bep_state.clone()));

    // We use this to ensure that the device id provided by the connection is assigned only by a
    // sigle client at a time.
    let device_id_assigner: Arc<tokio::sync::Mutex<bool>> = Arc::new(tokio::sync::Mutex::new(true));

    let _bg = if let Some(m) = grizol_config.mountpoint.as_ref() {
        let fs = GrizolFS::new(grizol_config.clone(), bep_state.clone());
        Some(fuse::mount(m, fs))
    } else {
        None
    };

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
}

async fn handle_incoming_data(
    bep_processor: Arc<BepProcessor<UtcTimeSource>>,
    tcp_stream: TcpStream,
    acceptor: TlsAcceptor,
    client_device_id: Arc<Mutex<Option<DeviceId>>>,
    device_id_assigner: Arc<tokio::sync::Mutex<bool>>,
) -> io::Result<()> {
    let (tls_stream, cdid) = {
        // we take the lock for device_id_assigner to ensure that no other thread will write the
        // client device id at the same time.
        let _ = device_id_assigner.lock().await;
        let tls_stream = acceptor.accept(tcp_stream).await?;
        let cdid: Option<DeviceId> = { *client_device_id.lock().unwrap() };
        (tls_stream, cdid.unwrap())
    };
    debug!("Received a connection from device: {:?}", cdid);

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
            tokio::time::sleep(PING_INTERVAL).await;

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
