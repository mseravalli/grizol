#[macro_use]
extern crate log;

mod connectivity;
mod core;

use crate::connectivity::ServerConfigArgs;
use crate::connectivity::TlsServer;
use crate::core::BepProcessor;
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

fn main() {
    // let version = env!("CARGO_PKG_NAME").to_string() + ", version: " + env!("CARGO_PKG_VERSION");

    env_logger::init();

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
            match event.token() {
                LISTENER => {
                    let bep_processor = BepProcessor::default();
                    tlsserv
                        .accept(poll.registry(), bep_processor)
                        .expect("error accepting socket");
                }
                _ => {
                    tlsserv.conn_event(poll.registry(), event);
                }
            }
        }
    }
}
