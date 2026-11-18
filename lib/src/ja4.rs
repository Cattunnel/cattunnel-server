//! The JA4 fingerprint of a ClientHello.
//!
//! A port of the C++ implementation in the client (`net/test/ja4.cpp`), so the
//! server counts the same fingerprints the client tests are pinned to. The
//! fingerprint shows what a client looks like from the outside: a browser, an
//! old phone, our own client. Counting them tells how many of each we serve.
//!
//! See <https://github.com/FoxIO-LLC/ja4> for the description.

use ring::digest;
use std::collections::HashMap;
use std::sync::Mutex;

/// The ClientHello as the server sees it.
pub(crate) struct ClientHelloInfo<'a> {
    /// The legacy version of the ClientHello.
    pub version: u16,
    /// The cipher suites the client offers.
    pub ciphers: &'a [u16],
    /// The raw extension block of the ClientHello.
    pub extensions: &'a [u8],
}

/// The counts of the fingerprints, by fingerprint.
static COUNTS: Mutex<Option<HashMap<String, u64>>> = Mutex::new(None);

const EXT_SERVER_NAME: u16 = 0;
const EXT_SIG_ALGS: u16 = 13;
const EXT_ALPN: u16 = 16;
const EXT_SUPPORTED_VERSIONS: u16 = 43;

/// Is the value a GREASE one? Such values are random and carry no meaning.
fn is_grease(value: u16) -> bool {
    ((value >> 8) == (value & 0xff)) && (((value >> 8) & 0x0f) == 0x0a)
}

/// The list of the extension types, the values of the ones we need, and whether
/// the server name is there.
struct Extensions {
    types: Vec<u16>,
    sni_present: bool,
    alpn: Vec<String>,
    sig_algs: Vec<u16>,
    supported_versions: Vec<u16>,
}

fn parse_extensions(data: &[u8]) -> Extensions {
    let mut out = Extensions {
        types: Vec::new(),
        sni_present: false,
        alpn: Vec::new(),
        sig_algs: Vec::new(),
        supported_versions: Vec::new(),
    };

    let mut pos = 0usize;
    while pos + 4 <= data.len() {
        let ext_type = u16::from_be_bytes([data[pos], data[pos + 1]]);
        let ext_len = u16::from_be_bytes([data[pos + 2], data[pos + 3]]) as usize;
        pos += 4;
        if pos + ext_len > data.len() {
            break;
        }
        let ext = &data[pos..pos + ext_len];
        pos += ext_len;
        out.types.push(ext_type);

        match ext_type {
            EXT_SERVER_NAME => out.sni_present = true,
            EXT_ALPN => {
                if ext.len() < 2 {
                    continue;
                }
                let list_len = u16::from_be_bytes([ext[0], ext[1]]) as usize;
                let mut p = 2usize;
                let end = (2 + list_len).min(ext.len());
                while p < end {
                    let len = ext[p] as usize;
                    p += 1;
                    if p + len > ext.len() {
                        break;
                    }
                    out.alpn
                        .push(String::from_utf8_lossy(&ext[p..p + len]).to_string());
                    p += len;
                }
            }
            EXT_SIG_ALGS => {
                if ext.len() < 2 {
                    continue;
                }
                let list_len = u16::from_be_bytes([ext[0], ext[1]]) as usize;
                let mut p = 2usize;
                let end = (2 + list_len).min(ext.len());
                while p + 2 <= end {
                    out.sig_algs.push(u16::from_be_bytes([ext[p], ext[p + 1]]));
                    p += 2;
                }
            }
            EXT_SUPPORTED_VERSIONS => {
                if ext.is_empty() {
                    continue;
                }
                let list_len = ext[0] as usize;
                let mut p = 1usize;
                let end = (1 + list_len).min(ext.len());
                while p + 2 <= end {
                    out.supported_versions
                        .push(u16::from_be_bytes([ext[p], ext[p + 1]]));
                    p += 2;
                }
            }
            _ => {}
        }
    }

    out
}

fn hex_list(values: &[u16]) -> String {
    values
        .iter()
        .map(|v| format!("{v:04x}"))
        .collect::<Vec<_>>()
        .join(",")
}

fn sha256_prefix(data: &str) -> String {
    let digest = digest::digest(&digest::SHA256, data.as_bytes());
    hex::encode(digest)[..12].to_string()
}

/// The JA4 fingerprint of a ClientHello, an empty string if it cannot be built.
pub(crate) fn compute(hello: &ClientHelloInfo<'_>) -> String {
    let ext = parse_extensions(hello.extensions);

    let mut ciphers: Vec<u16> = hello
        .ciphers
        .iter()
        .copied()
        .filter(|v| !is_grease(*v))
        .collect();
    let mut ext_types: Vec<u16> = ext
        .types
        .iter()
        .copied()
        .filter(|v| !is_grease(*v))
        .collect();
    let sig_algs: Vec<u16> = ext
        .sig_algs
        .iter()
        .copied()
        .filter(|v| !is_grease(*v))
        .collect();
    let versions: Vec<u16> = ext
        .supported_versions
        .iter()
        .copied()
        .filter(|v| !is_grease(*v))
        .collect();

    ciphers.sort_unstable();
    ext_types.sort_unstable();

    let version = versions.iter().copied().max().unwrap_or(hello.version);
    let version_name = match version {
        0x0304 => "13",
        0x0303 => "12",
        0x0302 => "11",
        0x0301 => "10",
        0x0300 => "s3",
        0x0200 => "s2",
        0x0100 => "s1",
        _ => "00",
    };

    let mut out = String::with_capacity(36);
    out.push('t');
    out.push_str(version_name);
    out.push(if ext.sni_present { 'd' } else { 'i' });
    out.push_str(&format!("{:02}", ciphers.len().min(99)));
    out.push_str(&format!("{:02}", ext_types.len().min(99)));
    if let Some(first) = ext.alpn.first() {
        if !first.is_empty() {
            let mut chars = first.chars();
            out.push(chars.next().unwrap());
            out.push(chars.last().unwrap());
        } else {
            out.push_str("00");
        }
    } else {
        out.push_str("00");
    }

    out.push('_');
    out.push_str(&sha256_prefix(&hex_list(&ciphers)));

    ext_types.retain(|v| *v != EXT_SERVER_NAME && *v != EXT_ALPN);
    let mut sha_input = hex_list(&ext_types);
    if !sig_algs.is_empty() {
        sha_input.push('_');
        sha_input.push_str(&hex_list(&sig_algs));
    }
    out.push('_');
    out.push_str(&sha256_prefix(&sha_input));

    out
}

/// Count a fingerprint.
pub(crate) fn count(ja4: &str) {
    if ja4.is_empty() {
        return;
    }
    let mut guard = COUNTS.lock().unwrap_or_else(|e| e.into_inner());
    let counts = guard.get_or_insert_with(HashMap::new);
    *counts.entry(ja4.to_string()).or_insert(0) += 1;
}

/// The counted fingerprints and their counts.
pub(crate) fn snapshot() -> Vec<(String, u64)> {
    let guard = COUNTS.lock().unwrap_or_else(|e| e.into_inner());
    let mut out: Vec<(String, u64)> = guard
        .as_ref()
        .map(|c| c.iter().map(|(k, v)| (k.clone(), *v)).collect())
        .unwrap_or_default();
    out.sort();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grease_is_detected() {
        assert!(is_grease(0x0a0a));
        assert!(is_grease(0xfafa));
        assert!(!is_grease(0xca04));
        assert!(!is_grease(0x1301));
    }

    #[test]
    fn extensions_are_parsed() {
        // server_name, ALPN with h2, supported versions with TLS 1.3
        let mut data: Vec<u8> = Vec::new();
        data.extend_from_slice(&EXT_SERVER_NAME.to_be_bytes());
        data.extend_from_slice(&0u16.to_be_bytes());
        data.extend_from_slice(&EXT_ALPN.to_be_bytes());
        data.extend_from_slice(&5u16.to_be_bytes());
        data.extend_from_slice(&[0, 3, 2, b'h', b'2']);
        data.extend_from_slice(&EXT_SUPPORTED_VERSIONS.to_be_bytes());
        data.extend_from_slice(&3u16.to_be_bytes());
        data.extend_from_slice(&[2, 0x03, 0x04]);

        let ext = parse_extensions(&data);
        assert!(ext.sni_present);
        assert_eq!(ext.alpn, vec!["h2".to_string()]);
        assert_eq!(ext.supported_versions, vec![0x0304]);
    }

    #[test]
    fn count_and_snapshot() {
        count("t13d1516h2_000000000000_000000000000");
        count("t13d1516h2_000000000000_000000000000");
        let snapshot = snapshot();
        let found = snapshot
            .iter()
            .find(|(k, _)| k == "t13d1516h2_000000000000_000000000000");
        assert!(found.is_some());
        assert!(found.unwrap().1 >= 2);
    }
}
