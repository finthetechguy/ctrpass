//! What each peer MAC has exchanged, the MAC filter, and blocked MACs.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::disk;
use crate::Error;
use crate::wifi::frame::Mac;

/// How many MACs the filter remembers, oldest first out.
pub const FILTER_MAX: usize = 255;
/// Seconds to wait before engaging a peer again after an exchange that did not finish.
pub const RETRY: u64 = 60;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Peer {
    pub first: u64,
    pub last: u64,
    pub exchanges: u32,
    pub attempts: u32,
    pub last_attempt: u64,
    pub sent: BTreeSet<u64>,
    pub received: BTreeSet<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Block {
    pub when: u64,
    pub note: String,
}

#[derive(Default, Serialize, Deserialize)]
struct Book {
    #[serde(default)]
    peers: BTreeMap<String, Peer>,
    #[serde(default)]
    blocked: BTreeMap<String, Block>,
    /// MACs that completed an exchange, refused until removed or pushed out.
    #[serde(default)]
    filter: Vec<String>,
}

pub struct PeerBook {
    path: PathBuf,
    book: Book,
}

pub fn mac_key(mac: &Mac) -> String {
    mac.iter()
        .map(|b| format!("{b:02x}"))
        .collect::<Vec<_>>()
        .join(":")
}

impl PeerBook {
    /// Loads the book at `path`, starting empty if it is missing or unreadable.
    pub fn open(path: &Path) -> Result<Self, Error> {
        let book = match fs::read(path) {
            Ok(d) => serde_json::from_slice(&d).unwrap_or_default(),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Book::default(),
            Err(e) => return Err(e.into()),
        };
        Ok(PeerBook {
            path: path.to_path_buf(),
            book,
        })
    }

    fn save(&self) -> Result<(), Error> {
        let data = serde_json::to_vec_pretty(&self.book).map_err(io::Error::other)?;
        disk::write_atomic(&self.path, &data)?;
        Ok(())
    }

    pub fn peers(&self) -> impl Iterator<Item = (&str, &Peer)> {
        self.book.peers.iter().map(|(k, v)| (k.as_str(), v))
    }

    pub fn peer(&self, mac: &Mac) -> Option<&Peer> {
        self.book.peers.get(&mac_key(mac))
    }

    pub fn filter(&self) -> &[String] {
        &self.book.filter
    }

    pub fn blocked(&self) -> impl Iterator<Item = (&str, &Block)> {
        self.book.blocked.iter().map(|(k, v)| (k.as_str(), v))
    }

    pub fn is_filtered(&self, mac: &Mac) -> bool {
        self.book.filter.contains(&mac_key(mac))
    }

    pub fn is_blocked(&self, mac: &Mac) -> bool {
        self.book.blocked.contains_key(&mac_key(mac))
    }

    /// Removes one MAC from the filter, or all of them, returning how many were removed.
    pub fn unfilter(&mut self, mac: Option<&Mac>) -> Result<usize, Error> {
        let before = self.book.filter.len();
        match mac {
            Some(m) => {
                let k = mac_key(m);
                self.book.filter.retain(|f| *f != k);
            }
            None => self.book.filter.clear(),
        }
        self.save()?;
        Ok(before - self.book.filter.len())
    }

    pub fn block(&mut self, mac: &Mac, note: &str, now: u64) -> Result<(), Error> {
        self.book.blocked.insert(
            mac_key(mac),
            Block {
                when: now,
                note: note.into(),
            },
        );
        self.save()
    }

    pub fn unblock(&mut self, mac: &Mac) -> Result<bool, Error> {
        let gone = self.book.blocked.remove(&mac_key(mac)).is_some();
        self.save()?;
        Ok(gone)
    }

    /// Seconds left before a peer whose last exchange failed may be engaged again.
    pub fn wait(&self, mac: &Mac, now: u64) -> u64 {
        match self.peer(mac) {
            Some(p) if p.last_attempt > 0 => (p.last_attempt + RETRY).saturating_sub(now),
            _ => 0,
        }
    }

    /// Whether a peer may be engaged now.
    pub fn due(&self, mac: &Mac, now: u64) -> bool {
        !self.is_filtered(mac) && !self.is_blocked(mac) && self.wait(mac, now) == 0
    }

    fn entry(&mut self, mac: &Mac, now: u64) -> &mut Peer {
        self.book.peers.entry(mac_key(mac)).or_insert_with(|| Peer {
            first: now,
            ..Peer::default()
        })
    }

    /// Records an exchange that did not finish.
    pub fn note_attempt(&mut self, mac: &Mac, now: u64) -> Result<(), Error> {
        let p = self.entry(mac, now);
        p.last_attempt = now;
        p.attempts += 1;
        self.save()
    }

    /// Records a finished exchange and adds the peer to the filter.
    pub fn record(
        &mut self,
        mac: &Mac,
        sent: &[u64],
        received: &[u64],
        now: u64,
    ) -> Result<(), Error> {
        let p = self.entry(mac, now);
        p.last = now;
        p.exchanges += 1;
        p.last_attempt = 0;
        p.sent.extend(sent);
        p.received.extend(received);
        let k = mac_key(mac);
        if !self.book.filter.contains(&k) {
            self.book.filter.push(k);
            let over = self.book.filter.len().saturating_sub(FILTER_MAX);
            self.book.filter.drain(..over);
        }
        self.save()
    }

    /// Drops one peer's history and filter entry, or everyone's, returning how many peers were dropped.
    pub fn forget(&mut self, mac: Option<&Mac>) -> Result<usize, Error> {
        let n = match mac {
            Some(m) => {
                let k = mac_key(m);
                self.book.filter.retain(|f| *f != k);
                usize::from(self.book.peers.remove(&k).is_some())
            }
            None => {
                self.book.filter.clear();
                std::mem::take(&mut self.book.peers).len()
            }
        };
        self.save()?;
        Ok(n)
    }

    pub fn already_sent(&self, mac: &Mac, id: u64) -> bool {
        self.peer(mac).is_some_and(|p| p.sent.contains(&id))
    }

    pub fn came_from(&self, mac: &Mac, id: u64) -> bool {
        self.peer(mac).is_some_and(|p| p.received.contains(&id))
    }
}
