use crate::device_id::DeviceId;
use crate::grizol;

use rustls::server::{ClientCertVerified, ClientCertVerifier};
use rustls::Certificate;
use rustls::DistinguishedName;
use rustls::{self};

use std::collections::HashSet;
use std::fs;
use std::io::BufReader;

use std::sync::Arc;
use std::sync::Mutex;
use std::time::SystemTime;

const BEP_PROTOCOL_ID: &str = "bep/1.0";

struct PresharedAuth {
    subjects: Vec<DistinguishedName>,
    trusted_peers: HashSet<DeviceId>,
    client_device_id: Arc<Mutex<Option<DeviceId>>>,
}

impl PresharedAuth {
    #[inline(always)]
    fn boxed(
        trusted_peers: HashSet<DeviceId>,
        client_device_id: Arc<Mutex<Option<DeviceId>>>,
    ) -> Arc<dyn ClientCertVerifier> {
        // This function is needed because `ClientCertVerifier` is only reachable if the
        // `dangerous_configuration` feature is enabled, which makes coercing hard to outside users
        Arc::new(Self {
            subjects: Default::default(),
            trusted_peers,
            client_device_id,
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
        _intermediates: &[Certificate],
        _now: SystemTime,
    ) -> Result<ClientCertVerified, rustls::Error> {
        trace!("verifying the certificate");
        let client_id = DeviceId::from(end_entity);
        {
            let mut cdid = self.client_device_id.lock().unwrap();
            *cdid = Some(client_id);
        }

        let any_match = self
            .trusted_peers
            .iter()
            .any(|trusted_peer| &client_id == trusted_peer);
        if any_match {
            Ok(ClientCertVerified::assertion())
        } else {
            error!(
                "Unknown client device id '{}' will not be accepted",
                client_id.to_string()
            );
            Err(rustls::Error::InvalidCertificate(
                rustls::CertificateError::InvalidPurpose,
            ))
        }
    }
}

pub fn server_config(
    config: grizol::Config,
    client_device_id: Arc<Mutex<Option<DeviceId>>>,
) -> rustls::ServerConfig {
    let trusted_peers = config
        .trusted_peers
        .iter()
        .map(|x| DeviceId::try_from(x.as_str()).unwrap())
        .collect();
    let client_auth = PresharedAuth::boxed(trusted_peers, client_device_id);

    let suites = rustls::ALL_CIPHER_SUITES.to_vec();

    let versions = rustls::ALL_VERSIONS.to_vec();

    let certs = load_certs(&config.cert);
    let privkey = load_private_key(&config.key);

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

// impl Into<rustls::ServerConfig> for grizol::Config {
//     fn into(self) -> rustls::ServerConfig {
//         let trusted_peers = self
//             .trusted_peers
//             .iter()
//             .map(|x| DeviceId::try_from(x.as_str()).unwrap())
//             .collect();
//         let client_auth = PresharedAuth::boxed(trusted_peers);

//         let suites = rustls::ALL_CIPHER_SUITES.to_vec();

//         let versions = rustls::ALL_VERSIONS.to_vec();

//         let certs = load_certs(&self.cert);
//         let privkey = load_private_key(&self.key);

//         let mut config = rustls::ServerConfig::builder()
//             .with_cipher_suites(&suites)
//             .with_safe_default_kx_groups()
//             .with_protocol_versions(&versions)
//             .expect("inconsistent cipher-suites/versions specified")
//             .with_client_cert_verifier(client_auth)
//             .with_single_cert_with_ocsp_and_sct(certs, privkey, vec![], vec![])
//             .expect("bad certificates/private key");

//         config.key_log = Arc::new(rustls::KeyLogFile::new());

//         config.alpn_protocols = vec![BEP_PROTOCOL_ID.as_bytes().to_vec()];

//         config
//     }
// }

fn load_certs(filename: &str) -> Vec<rustls::Certificate> {
    let certfile = fs::File::open(filename)
        .expect(format!("cannot open certificate file at {}", filename).as_str());
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
