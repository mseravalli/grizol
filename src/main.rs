#[macro_use]
extern crate log;

mod connectivity;
mod core;

use crate::connectivity::ServerConfigArgs;
use crate::connectivity::TlsServer;
use crate::core::process_incoming_message;
use actix::prelude::*;
use clap::Parser;
use mio::net::TcpListener;
use rustls::server::{
    AllowAnyAnonymousOrAuthenticatedClient, AllowAnyAuthenticatedClient, NoClientAuth,
};
use rustls::{self, RootCertStore};
use std::fs;
use std::io::{BufReader, Read, Write};
use std::net;
use std::sync::Arc;

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

#[actix_rt::main]
async fn main() {
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

    loop {
        poll.poll(&mut events, None).unwrap();

        for event in events.iter() {
            trace!("Received event");
            match event.token() {
                LISTENER => {
                    tlsserv
                        .accept(poll.registry())
                        .expect("error accepting socket");
                }
                token => {
                    let received_event = tlsserv.process_event(poll.registry(), event);
                    match received_event {
                        Ok(buf) => {
                            // TODO: better handle None case
                            tlsserv
                                .get_connection(&token)
                                .map(|c| process_incoming_message(&buf, c));
                        }
                        Err(e) => {
                            trace!("Received event was {}", e);
                        }
                    };
                }
            }
        }
    }
    System::current().stop();
}
