#[macro_use]
extern crate log;

mod connectivity;
mod core;
mod device_id;

use crate::connectivity::ServerConfigArgs;
use crate::connectivity::{OpenConnection, TlsServer};
use crate::core::{BepAction, BepProcessor};
use crate::device_id::DeviceId;
use actix::prelude::*;
use clap::Parser;
use data_encoding::BASE32;
use mio::net::TcpListener;
use rustls::{self, RootCertStore};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::collections::HashSet;
use std::fs;
use std::io::{BufReader, Read, Write};
use std::net;
use std::sync::mpsc::channel;
use std::sync::mpsc::{Receiver, Sender};
use std::sync::Arc;
use std::thread;
use std::thread::JoinHandle;
use std::time::{Duration, Instant, UNIX_EPOCH};

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
    handle: JoinHandle<OpenConnection>,
    sender: Sender<BepAction>,
}

impl ClientData {
    fn new(handle: JoinHandle<OpenConnection>, sender: Sender<BepAction>) -> Self {
        ClientData { handle, sender }
    }
}

fn clean_finished_threads(clients_data: &mut HashMap<mio::Token, ClientData>, poll: &mio::Poll) {
    let finished_threads: Vec<mio::Token> = clients_data
        .iter()
        .filter(|x| x.1.handle.is_finished())
        .map(|x| x.0.clone())
        .collect();

    for token in finished_threads.iter() {
        let client_data = clients_data.remove(token).unwrap();
        let mut c = client_data.handle.join().unwrap();
        c.deregister(poll.registry());
    }
}

// FIXME: remove in favor or DeviceId::from
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

fn main() {
    // let version = env!("CARGO_PKG_NAME").to_string() + ", version: " + env!("CARGO_PKG_VERSION");
    setup_logging();

    let args: Args = Args::parse();

    let mut addr: net::SocketAddr = "0.0.0.0:443".parse().unwrap();
    addr.set_port(args.port.unwrap_or(443));
    let device_id = device_id_from_cert(&args.certs);

    let server_config_args: ServerConfigArgs = args.into();
    let config = server_config_args.into();

    let mut listener = TcpListener::bind(addr).expect("cannot listen on port");
    let mut poll = mio::Poll::new().unwrap();
    poll.registry()
        .register(&mut listener, LISTENER, mio::Interest::READABLE)
        .unwrap();

    let mut tlsserv = TlsServer::new(listener, config);
    info!("Created tls server");

    let mut events = mio::Events::with_capacity(256);

    let mut clients_data: HashMap<mio::Token, ClientData> = Default::default();

    loop {
        poll.poll(&mut events, Some(Duration::from_millis(100)))
            .unwrap();

        for event in events.iter() {
            trace!("Received event");
            match event.token() {
                LISTENER => {
                    let (mut connection, token) = tlsserv
                        .accept(poll.registry())
                        .expect("error accepting socket");

                    connection.register(poll.registry());

                    let (sender, receiver) = channel();

                    let handler = thread::spawn(move || {
                        let bep_processor = BepProcessor::new(device_id, connection, receiver);
                        bep_processor.run()
                    });

                    clients_data.insert(token, ClientData::new(handler, sender));
                }
                token => match clients_data.get(&token).as_ref().map(|x| &x.sender) {
                    Some(sender) => {
                        sender.send(BepAction::ReadClientMessage);
                    }
                    None => {
                        debug!("No sender for token: {:?}", token);
                    }
                },
            }
        }

        clean_finished_threads(&mut clients_data, &poll);
    }
}
