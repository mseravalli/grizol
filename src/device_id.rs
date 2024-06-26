use data_encoding::BASE32;
use sha2::{Digest, Sha256};
use std::convert::From;
use std::convert::Into;
use std::convert::TryFrom;
use std::fmt;
use std::fs::File;
use std::io::BufReader;
use std::path::Path;

const DEVICE_ID_LEN: usize = 32;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Ord, PartialOrd)]
pub struct DeviceId {
    pub id: [u8; DEVICE_ID_LEN],
}

impl fmt::Display for DeviceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let res = BASE32.encode(&self.id);
        let res = res.trim_end_matches('=');
        let res = luhnify(res).expect("It must always be possible to lunhify");
        write!(f, "{}", chunkify(&res))
    }
}

impl fmt::Debug for DeviceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "DeviceId \"{}\"", self)
    }
}

impl TryFrom<&str> for DeviceId {
    type Error = String;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        if !value.is_ascii() {
            return Err("Invalid string provided, only ASCII characters allowed".to_string());
        }

        let value = value
            .to_uppercase()
            // remove chunks
            .replace(['-', ' '], "")
            // replace possible typos
            .replace('0', "O")
            .replace('1', "I")
            .replace('8', "B");
        let value = unluhnify(&value)?;
        let dec = BASE32
            .decode((value + "====").as_bytes())
            .map_err(|x| x.to_string())?;

        let mut id: [u8; DEVICE_ID_LEN] = Default::default();
        id[..].copy_from_slice(&dec[..]);
        Ok(DeviceId { id })
    }
}

impl From<&rustls::Certificate> for DeviceId {
    fn from(cert: &rustls::Certificate) -> Self {
        let mut hasher = Sha256::new();

        hasher.update(&cert.0);

        let id: [u8; DEVICE_ID_LEN] = hasher.finalize().into();

        DeviceId { id }
    }
}

impl From<&Path> for DeviceId {
    fn from(path: &Path) -> Self {
        let certfile = File::open(path).expect("cannot open certificate file");
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
}

impl TryFrom<&[u8]> for DeviceId {
    type Error = String;

    fn try_from(input: &[u8]) -> Result<Self, Self::Error> {
        if input.len() != DEVICE_ID_LEN {
            return Err(format!(
                "Expected a Vec<u8> of len {}, and got instead: {}",
                DEVICE_ID_LEN,
                input.len()
            ));
        }

        let id: [u8; DEVICE_ID_LEN] = input[..DEVICE_ID_LEN]
            .try_into()
            .map_err(|e| format!("{:?}", e))?;

        Ok(DeviceId { id })
    }
}

impl TryFrom<&Vec<u8>> for DeviceId {
    type Error = String;

    fn try_from(input: &Vec<u8>) -> Result<Self, Self::Error> {
        DeviceId::try_from(&input[..])
    }
}

impl From<DeviceId> for Vec<u8> {
    fn from(val: DeviceId) -> Self {
        val.id.into()
    }
}

impl From<&DeviceId> for Vec<u8> {
    fn from(val: &DeviceId) -> Self {
        val.id.into()
    }
}

impl From<DeviceId> for [u8; DEVICE_ID_LEN] {
    fn from(val: DeviceId) -> Self {
        val.id
    }
}

impl From<&DeviceId> for [u8; DEVICE_ID_LEN] {
    fn from(val: &DeviceId) -> Self {
        val.id
    }
}

impl From<&DeviceId> for u64 {
    fn from(val: &DeviceId) -> Self {
        u64::from_be_bytes(val.id[0..8].try_into().unwrap())
    }
}

fn chunkify(s: &str) -> String {
    let chunks = s.len() / 7;

    let s: Vec<char> = s.chars().collect();
    let mut res: Vec<char> = vec!['0'; chunks * 8 - 1];

    for i in 0..chunks {
        if i > 0 {
            res[i * 8 - 1] = '-';
        }

        let chunk_end = std::cmp::min((i + 1) * 7, s.len());
        let chunk = &s[i * 7..chunk_end];
        res[i * 8..i * 8 + chunk.len()].copy_from_slice(chunk);
    }
    String::from_iter(res.iter())
}

fn luhnify(s: &str) -> Result<String, String> {
    if s.len() != 52 {
        return Err(format!("{}: unsupported string length {}", s, s.len()));
    }

    let s: Vec<char> = s.chars().collect();
    let mut res: [char; 56] = ['0'; 56];
    for i in 0..4 {
        let p = &s[i * 13..(i + 1) * 13];
        res[i * 14..(i + 1) * 14 - 1].copy_from_slice(p);
        let l = luhn32(p)?;
        res[(i + 1) * 14 - 1] = l;
    }
    Ok(String::from_iter(res.iter()))
}

fn unluhnify(s: &str) -> Result<String, String> {
    if s.len() != 56 {
        return Err(format!("{}: unsupported string length {}", s, s.len()));
    }

    let mut res: [char; 52] = ['0'; 52];

    let s: Vec<char> = s.chars().collect();

    for i in 0..4 {
        let p = &s[i * (13 + 1)..(i + 1) * (13 + 1) - 1];
        res[i * 13..(i + 1) * 13].copy_from_slice(p);
        let l = luhn32(p)?;
        let check_pos = (i + 1) * 14 - 1;
        if s[check_pos] != l {
            return Err(format!(
                "Incorrect check digit at position {}. Was {}, expected {}.",
                check_pos, s[check_pos], l
            ));
        }
    }

    Ok(String::from_iter(res.iter()))
}

const LUHN_BASE32: [char; 32] = [
    'A', 'B', 'C', 'D', 'E', 'F', 'G', 'H', 'I', 'J', 'K', 'L', 'M', 'N', 'O', 'P', 'Q', 'R', 'S',
    'T', 'U', 'V', 'W', 'X', 'Y', 'Z', '2', '3', '4', '5', '6', '7',
];

fn codepoint32(b: char) -> Result<u32, String> {
    // TODO: see if we can use match here instead
    if b.is_ascii_uppercase() {
        Ok(u32::from(b) - u32::from('A'))
    } else if ('2'..='7').contains(&b) {
        Ok(u32::from(b) + 26 - u32::from('2'))
    } else {
        Err(format!("Invalid char {} in alphabet {:?}", b, LUHN_BASE32))
    }
}

/// luhn32 returns a check digit for the string s, which should be composed
/// of characters from the alphabet [luhnBase32].
/// Doesn't follow the actual Luhn algorithm
/// see https://forum.syncthing.net/t/v0-9-0-new-node-id-format/478/6 for more.
fn luhn32(s: &[char]) -> Result<char, String> {
    let mut factor = 1;
    let mut sum = 0;
    let n = 32;

    for c in s.iter() {
        let codepoint = codepoint32(*c)?;
        let mut addend = factor * codepoint;
        factor = if factor == 2 { 1 } else { 2 };
        addend = (addend / n) + (addend % n);
        sum += addend;
    }
    let remainder = sum % n;
    let check_codepoint = usize::try_from((n - remainder) % n).map_err(|e| e.to_string())?;
    Ok(LUHN_BASE32[check_codepoint])
}
