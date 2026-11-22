//! A store's whole contents as one JSON value, for handing to a process that may not open the store itself.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{Mailbox, PEERS_FILE, Store, disk, peers};
use crate::Error;
use crate::cec::container::Container;
use crate::streetpass::service::{self, Service};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    /// The identity seed in hex.
    pub seed: String,
    pub titles: Vec<TitleState>,
    pub messages: Vec<MessageState>,
    /// The peer book as stored.
    #[serde(default)]
    pub peers: Value,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TitleState {
    /// The title ID in hex.
    pub title: String,
    #[serde(default)]
    pub name: Option<String>,
    /// The signing key in hex.
    #[serde(default)]
    pub key: Option<String>,
    #[serde(default)]
    pub receive: bool,
    /// A chosen service record in hex.
    #[serde(default)]
    pub service: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MessageState {
    #[serde(rename = "box")]
    pub mailbox: String,
    /// The whole message in hex.
    pub data: String,
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn unhex(what: &str, s: &str) -> Result<Vec<u8>, Error> {
    crate::title::unhex(s).ok_or_else(|| Error::Format(format!("{what} is not hex")))
}

fn title_id(s: &str) -> Result<u32, Error> {
    u32::from_str_radix(s.trim_start_matches("0x"), 16)
        .map_err(|_| Error::Format(format!("title {s:?} is not hex")))
}

pub fn mailbox_named(name: &str) -> Result<Mailbox, Error> {
    Mailbox::ALL
        .into_iter()
        .find(|m| m.name() == name)
        .ok_or_else(|| Error::Format(format!("no box named {name:?}")))
}

impl Store {
    /// The peer book as JSON, empty when none was kept.
    pub fn peers_json(&self) -> Result<Value, Error> {
        match std::fs::read(self.root.join(PEERS_FILE)) {
            Ok(d) => Ok(serde_json::from_slice(&d).unwrap_or_default()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Value::Null),
            Err(e) => Err(e.into()),
        }
    }

    /// Replaces the peer book with one given as JSON.
    pub fn set_peers_json(&mut self, v: &Value) -> Result<(), Error> {
        if v.is_null() {
            return Ok(());
        }
        peers::check(v)?;
        let data = serde_json::to_vec_pretty(v).map_err(std::io::Error::other)?;
        disk::write_atomic(&self.root.join(PEERS_FILE), &data)?;
        Ok(())
    }

    pub fn snapshot(&self) -> Result<Snapshot, Error> {
        let receive = self.receive_titles();
        let titles = self
            .titles()
            .iter()
            .map(|&t| TitleState {
                title: format!("{t:08X}"),
                name: self.title_name(t),
                key: self.title_key(t).map(|k| hex(&k)),
                receive: receive.contains(&t),
                service: self.service(t).map(|s| hex(&service::encode(&[s]))),
            })
            .collect();
        let messages = self
            .messages(None, None)?
            .into_iter()
            .map(|s| MessageState {
                mailbox: s.mailbox.name().into(),
                data: hex(s.container.as_bytes()),
            })
            .collect();
        Ok(Snapshot {
            seed: hex(&self.seed()?),
            titles,
            messages,
            peers: self.peers_json()?,
        })
    }

    /// Fills an empty store from a snapshot.
    pub fn restore(&mut self, snap: &Snapshot) -> Result<(), Error> {
        let seed: [u8; 32] = unhex("seed", &snap.seed)?
            .try_into()
            .map_err(|_| Error::Format("seed is not 32 bytes".into()))?;
        self.set_seed(&seed)?;
        for t in &snap.titles {
            let id = title_id(&t.title)?;
            let key = match &t.key {
                Some(k) => Some(
                    unhex("key", k)?
                        .try_into()
                        .map_err(|_| Error::Format("key is not 32 bytes".into()))?,
                ),
                None => None,
            };
            self.add_title(id, t.name.as_deref(), key.as_ref(), true)?;
            self.set_receive(id, t.receive)?;
            if let Some(s) = &t.service {
                let (records, bad) = service::decode(&unhex("service", s)?);
                if let Some(e) = bad {
                    return Err(e);
                }
                let record: Option<&Service> = records.first();
                self.set_service(id, record)?;
            }
        }
        for m in &snap.messages {
            let c = Container::parse(&unhex("message", &m.data)?)?;
            self.add_message(&c, mailbox_named(&m.mailbox)?, None, None)?;
        }
        self.set_peers_json(&snap.peers)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cec::container::tests::{KEY, sample};

    #[test]
    fn round_trips() {
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        let mut s = Store::open(a.path()).unwrap();
        s.add_title(0x20800, Some("Mii Plaza"), Some(&KEY), false)
            .unwrap();
        s.add_message(&sample(0x20800, 1), Mailbox::Outbox, None, None)
            .unwrap();
        s.add_message(&sample(0x20800, 2), Mailbox::Inbox, None, None)
            .unwrap();
        s.set_receive(0x20800, true).unwrap();
        s.peer_book()
            .unwrap()
            .record(&[2, 0, 0, 0, 0, 1], &[1], &[], 100)
            .unwrap();
        let snap = s.snapshot().unwrap();
        let text = serde_json::to_string(&snap).unwrap();
        let mut t = Store::open(b.path()).unwrap();
        t.restore(&serde_json::from_str(&text).unwrap()).unwrap();
        assert_eq!(t.snapshot().unwrap(), snap);
        assert!(t.peer_book().unwrap().already_sent(&[2, 0, 0, 0, 0, 1], 1));
        assert!(t.set_peers_json(&serde_json::json!({"peers": 5})).is_err());
    }
}
