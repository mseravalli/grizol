#[macro_use]
extern crate log;

mod connectivity;
mod core;

use crate::connectivity::ServerConfigArgs;
use crate::connectivity::{OpenConnection, TlsServer};
use crate::core::BepProcessor;
use actix::prelude::*;
use clap::Parser;
use mio::net::TcpListener;
use rustls::server::{
    AllowAnyAnonymousOrAuthenticatedClient, AllowAnyAuthenticatedClient, NoClientAuth,
};
use rustls::{self, RootCertStore};
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

fn clean_finished_threads(
    handlers: &mut HashMap<mio::Token, JoinHandle<OpenConnection>>,
    token_senders: &mut HashMap<mio::Token, Sender<i32>>,
    poll: &mio::Poll,
) {
    let finished_threads: Vec<mio::Token> = handlers
        .iter()
        .filter(|x| x.1.is_finished())
        .map(|x| x.0.clone())
        .collect();

    for token in finished_threads.iter() {
        let handler = handlers.remove(token).unwrap();
        let mut c = handler.join().unwrap();
        c.deregister(poll.registry());
        token_senders.remove(token);
    }
}

fn main() {
    // let version = env!("CARGO_PKG_NAME").to_string() + ", version: " + env!("CARGO_PKG_VERSION");
    setup_logging();

    let args: Args = Args::parse();

    let mut addr: net::SocketAddr = "0.0.0.0:443".parse().unwrap();
    addr.set_port(args.port.unwrap_or(443));

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

    // TODO: unify these two maps into one
    let mut token_senders: HashMap<mio::Token, Sender<i32>> = Default::default();
    let mut handlers: HashMap<mio::Token, JoinHandle<OpenConnection>> = Default::default();

    loop {
        poll.poll(&mut events, None).unwrap();

        for event in events.iter() {
            trace!("Received event");
            match event.token() {
                LISTENER => {
                    let (mut connection, token) = tlsserv
                        .accept(poll.registry())
                        .expect("error accepting socket");

                    connection.register(poll.registry());

                    let (sender, receiver) = channel();

                    let handler =
                        thread::spawn(move || BepProcessor::process(connection, receiver));

                    token_senders.insert(token, sender);
                    handlers.insert(token, handler);
                }
                token => match token_senders.get(&token) {
                    Some(sender) => {
                        sender.send(10).unwrap();
                    }
                    None => {
                        debug!("No sender for token: {:?}", token);
                    }
                },
            }

            clean_finished_threads(&mut handlers, &mut token_senders, &poll);
        }
    }
}
