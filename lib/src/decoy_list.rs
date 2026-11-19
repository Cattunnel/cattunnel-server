//! Addresses that only ever see the decoy site.
//!
//! The firewall puts an address here after it probed a closed port. Such an
//! address still gets the TLS handshake and the decoy site, exactly like any
//! visitor without credentials, but every attempt to use the VPN from it is
//! answered as a failed authentication, even with valid credentials. The file
//! is plain text, one IP address or CIDR network per line, `#` starts a
//! comment. It is re-read when its modification time changes; a missing or
//! unreadable file means an empty list.

use ipnet::IpNet;
use std::collections::HashSet;
use std::net::IpAddr;
use std::path::PathBuf;
use std::sync::RwLock;
use std::time::{Duration, Instant, SystemTime};

/// How often the file's modification time is checked at most.
const RECHECK_INTERVAL: Duration = Duration::from_secs(5);

#[derive(Default)]
struct Entries {
    addresses: HashSet<IpAddr>,
    networks: Vec<IpNet>,
}

impl Entries {
    fn parse(text: &str) -> Self {
        let mut entries = Entries::default();
        for line in text.lines() {
            let item = line.split('#').next().unwrap_or("").trim();
            let Some(token) = item.split_whitespace().next() else {
                continue;
            };
            if let Ok(ip) = token.parse::<IpAddr>() {
                entries.addresses.insert(ip.to_canonical());
            } else if let Ok(net) = token.parse::<IpNet>() {
                entries.networks.push(net.trunc());
            }
        }
        entries
    }

    fn contains(&self, ip: IpAddr) -> bool {
        let ip = ip.to_canonical();
        self.addresses.contains(&ip) || self.networks.iter().any(|net| net.contains(&ip))
    }

    fn len(&self) -> usize {
        self.addresses.len() + self.networks.len()
    }
}

struct State {
    checked_at: Option<Instant>,
    modified: Option<SystemTime>,
    entries: Entries,
}

pub(crate) struct DecoyList {
    path: PathBuf,
    state: RwLock<State>,
}

impl DecoyList {
    pub(crate) fn new(path: impl Into<PathBuf>) -> Self {
        let list = Self {
            path: path.into(),
            state: RwLock::new(State {
                checked_at: None,
                modified: None,
                entries: Entries::default(),
            }),
        };
        list.refresh(true);
        list
    }

    /// Whether `ip` may only see the decoy site.
    pub(crate) fn contains(&self, ip: IpAddr) -> bool {
        self.refresh(false);
        self.state.read().unwrap().entries.contains(ip)
    }

    /// The number of addresses and networks currently loaded.
    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.state.read().unwrap().entries.len()
    }

    fn refresh(&self, force: bool) {
        if !force {
            let state = self.state.read().unwrap();
            if state
                .checked_at
                .is_some_and(|t| t.elapsed() < RECHECK_INTERVAL)
            {
                return;
            }
        }
        let mut state = self.state.write().unwrap();
        if !force
            && state
                .checked_at
                .is_some_and(|t| t.elapsed() < RECHECK_INTERVAL)
        {
            return;
        }
        state.checked_at = Some(Instant::now());
        let modified = std::fs::metadata(&self.path)
            .and_then(|m| m.modified())
            .ok();
        if !force && modified == state.modified {
            return;
        }
        state.modified = modified;
        state.entries = match std::fs::read_to_string(&self.path) {
            Ok(text) => Entries::parse(&text),
            Err(_) => Entries::default(),
        };
        log::info!(
            "Decoy-only list {}: {} entries",
            self.path.display(),
            state.entries.len()
        );
    }

    #[cfg(test)]
    fn expire_recheck(&self) {
        self.state.write().unwrap().checked_at = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn parses_addresses_networks_and_comments() {
        let entries = Entries::parse(
            "# probers\n203.0.113.7\n198.51.100.0/24 # a network\n\n2001:db8::1\nnot-an-ip\n10.0.0.300\n",
        );
        assert_eq!(entries.len(), 3);
        assert!(entries.contains("203.0.113.7".parse().unwrap()));
        assert!(entries.contains("198.51.100.200".parse().unwrap()));
        assert!(entries.contains("2001:db8::1".parse().unwrap()));
        assert!(!entries.contains("203.0.113.8".parse().unwrap()));
    }

    #[test]
    fn ipv4_mapped_client_address_matches_ipv4_entry() {
        let entries = Entries::parse("203.0.113.7\n");
        assert!(entries.contains("::ffff:203.0.113.7".parse().unwrap()));
    }

    #[test]
    fn missing_file_is_an_empty_list() {
        let list = DecoyList::new("/nonexistent/decoy-only.txt");
        assert_eq!(list.len(), 0);
        assert!(!list.contains("203.0.113.7".parse().unwrap()));
    }

    #[test]
    fn file_is_reloaded_when_it_changes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("decoy-only.txt");
        std::fs::write(&path, "203.0.113.7\n").unwrap();
        let list = DecoyList::new(&path);
        assert!(list.contains("203.0.113.7".parse().unwrap()));
        assert!(!list.contains("203.0.113.8".parse().unwrap()));

        // A new file with a different modification time replaces the list.
        std::thread::sleep(Duration::from_millis(20));
        let tmp = dir.path().join("decoy-only.txt.tmp");
        let mut f = std::fs::File::create(&tmp).unwrap();
        writeln!(f, "203.0.113.8").unwrap();
        drop(f);
        let later = SystemTime::now() + Duration::from_secs(2);
        std::fs::File::options()
            .write(true)
            .open(&tmp)
            .unwrap()
            .set_modified(later)
            .unwrap();
        std::fs::rename(&tmp, &path).unwrap();
        list.expire_recheck();
        assert!(list.contains("203.0.113.8".parse().unwrap()));
        assert!(!list.contains("203.0.113.7".parse().unwrap()));
    }
}
