use data_encoding::BASE32;
use sha2::{Digest, Sha256};
use std::convert::From;
use std::convert::Into;
use std::convert::TryFrom;

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct DeviceId {
    pub id: [u8; 32],
}

impl TryFrom<&str> for DeviceId {
    type Error = String;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        if !value.is_ascii() {
            return Err(format!(
                "Invalid string provided, only ASCII characters allowed"
            ));
        }

        let value = value
            .to_uppercase()
            // remove chunks
            .replace("-", "")
            .replace(" ", "")
            // replace possible typos
            .replace("0", "O")
            .replace("1", "I")
            .replace("8", "B");
        let value = unluhnify(&value)?;
        let dec = BASE32
            .decode((value + "====").as_bytes())
            .map_err(|x| x.to_string())?;

        let mut id: [u8; 32] = Default::default();
        id[..].copy_from_slice(&dec[..]);
        Ok(DeviceId { id })
    }
}

impl From<&rustls::Certificate> for DeviceId {
    fn from(cert: &rustls::Certificate) -> Self {
        let mut hasher = Sha256::new();

        hasher.update(&cert.0);

        let id: [u8; 32] = hasher.finalize().into();

        DeviceId { id }
    }
}

impl ToString for DeviceId {
    fn to_string(&self) -> String {
        let res = BASE32.encode(&self.id);
        let res = res.trim_end_matches("=");
        let res = luhnify(res).expect("It must always be possible to lunhify");
        chunkify(&res)
    }
}

impl Into<Vec<u8>> for DeviceId {
    fn into(self) -> Vec<u8> {
        self.id.into()
    }
}

impl Into<Vec<u8>> for &DeviceId {
    fn into(self) -> Vec<u8> {
        self.id.into()
    }
}

impl Into<[u8; 32]> for DeviceId {
    fn into(self) -> [u8; 32] {
        self.id
    }
}

impl Into<[u8; 32]> for &DeviceId {
    fn into(self) -> [u8; 32] {
        self.id
    }
}

impl Into<u64> for &DeviceId {
    fn into(self) -> u64 {
        u64::from_be_bytes(self.id[0..8].try_into().unwrap()).into()
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
        res[i * 13..(i + 1) * 13].copy_from_slice(&p);
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

const luhnBase32: [char; 32] = [
    'A', 'B', 'C', 'D', 'E', 'F', 'G', 'H', 'I', 'J', 'K', 'L', 'M', 'N', 'O', 'P', 'Q', 'R', 'S',
    'T', 'U', 'V', 'W', 'X', 'Y', 'Z', '2', '3', '4', '5', '6', '7',
];

fn codepoint32(b: char) -> Result<u32, String> {
    // TODO: see if we can use match here instead
    if 'A' <= b && b <= 'Z' {
        Ok(u32::from(b) - u32::from('A'))
    } else if '2' <= b && b <= '7' {
        Ok(u32::from(b) + 26 - u32::from('2'))
    } else {
        Err(format!("Invalid char {} in alphabet {:?}", b, luhnBase32))
    }
}

// luhn32 returns a check digit for the string s, which should be composed
// of characters from the alphabet luhnBase32.
// Doesn't follow the actual Luhn algorithm
// see https://forum.syncthing.net/t/v0-9-0-new-node-id-format/478/6 for more.
fn luhn32(s: &[char]) -> Result<char, String> {
    let mut factor = 1;
    let mut sum = 0;
    let n = 32;

    for i in 0..s.len() {
        let codepoint = codepoint32(s[i])?;
        let mut addend = factor * codepoint;
        factor = if factor == 2 { 1 } else { 2 };
        addend = (addend / n) + (addend % n);
        sum += addend;
    }
    let remainder = sum % n;
    let checkCodepoint = usize::try_from((n - remainder) % n).map_err(|e| e.to_string())?;
    Ok(char::from(luhnBase32[checkCodepoint]))
}
