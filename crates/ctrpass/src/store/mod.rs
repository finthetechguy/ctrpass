//! A CEC message folder laid out the way a console keeps it, with ctrpass's own state beside it.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::Error;
use crate::cec::container::Container;
use crate::identity::Seed;
use crate::keys::TitleKey;
use crate::streetpass::service::{self, Service};
use crate::wifi::frame::Mac;

pub(crate) mod disk;
pub mod peers;
pub mod snapshot;

pub use peers::PeerBook;
pub use snapshot::Snapshot;

pub const MAX_TITLES: usize = 24;
pub const INBOX_MAX_BYTES: usize = 0x10_0000;
pub const INBOX_MAX_MESSAGES: usize = 24;

const LIST_MAGIC: u16 = 0x6868;
const INFO_MAGIC: u16 = 0x6363;
const BOX_MAGIC: u16 = 0x6262;
const LIST_HEADER: usize = 12;
const LIST_ENTRY: usize = 16;
const LIST_LEN: usize = LIST_HEADER + MAX_TITLES * LIST_ENTRY;
const INFO_LEN: usize = 0x60;
const INFO_KEY: std::ops::Range<usize> = 0x10..0x30;
const BOX_INFO_LEN: usize = 0x20;

const CEC_DIR: &str = "CEC";
const LIST_FILE: &str = "MBoxList____";
const INFO_FILE: &str = "MBoxInfo____";
const NAME_FILE: &str = "MBoxData.010";
const BOX_INFO_FILE: &str = "BoxInfo_____";
const STATE_FILE: &str = "state.json";
const SEED_FILE: &str = "seed";
const PEERS_FILE: &str = "peers.json";
const LOCK_FILE: &str = "lock";
const DAMAGED_DIR: &str = "damaged";

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Mailbox {
    Inbox,
    Outbox,
}

impl Mailbox {
    pub const ALL: [Mailbox; 2] = [Mailbox::Inbox, Mailbox::Outbox];

    pub(crate) fn dir(self) -> &'static str {
        match self {
            Mailbox::Inbox => "InBox___",
            Mailbox::Outbox => "OutBox__",
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Mailbox::Inbox => "inbox",
            Mailbox::Outbox => "outbox",
        }
    }
}

/// Where a stored message came from.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Origin {
    /// Received from the console with this MAC.
    Peer(Mac),
    /// Written or imported on this machine, with a short note saying how.
    Local(String),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Provenance {
    pub origin: Origin,
    /// Seconds since the Unix epoch.
    pub when: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Stored {
    pub title: u32,
    pub mailbox: Mailbox,
    pub container: Container,
}

/// A file that could not be used and was moved into the damaged folder.
#[derive(Clone, Debug)]
pub struct Damaged {
    pub file: PathBuf,
    pub reason: String,
    pub moved_to: PathBuf,
}

/// What became of a batch of arrivals for one title.
#[derive(Debug, Default)]
pub struct Filed {
    pub filed: Vec<u64>,
    /// Message IDs that failed their title signature.
    pub refused: Vec<u64>,
}

#[derive(Default, Serialize, Deserialize)]
struct State {
    /// Titles whose MBoxInfo holds a real key, since an all-zero key is also valid.
    #[serde(default)]
    keyed: BTreeSet<u32>,
    #[serde(default)]
    receive: BTreeSet<u32>,
    /// Encoded service records advertised for titles that only receive.
    #[serde(default)]
    services: BTreeMap<u32, String>,
    #[serde(default)]
    origins: BTreeMap<String, Provenance>,
}

/// Files that cannot be read as messages, with the reason.
type Unusable = Vec<(PathBuf, String)>;

pub struct Store {
    root: PathBuf,
    cec: PathBuf,
    titles: Vec<u32>,
    state: State,
    damaged: Vec<Damaged>,
    _lock: File,
}

fn u16_at(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([b[at], b[at + 1]])
}

fn u32_at(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(b[at..at + 4].try_into().unwrap())
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

const BASE64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+-";

/// The file name a console gives a message: unpadded base64 of its ID with `-` for `/`.
pub fn message_file_name(id: u64) -> String {
    let bytes = id.to_le_bytes();
    let mut out = String::from("_");
    for chunk in bytes.chunks(3) {
        let mut n = [0u8; 3];
        n[..chunk.len()].copy_from_slice(chunk);
        let v = u32::from_be_bytes([0, n[0], n[1], n[2]]);
        for i in 0..=chunk.len() {
            out.push(BASE64[(v >> (18 - 6 * i) & 63) as usize] as char);
        }
    }
    out
}

fn origin_key(mailbox: Mailbox, id: u64) -> String {
    format!("{}/{id:016x}", mailbox.name())
}

fn decode_name(data: &[u8]) -> String {
    let units: Vec<u16> = data
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .take_while(|&u| u != 0)
        .collect();
    String::from_utf16_lossy(&units)
}

/// Chooses which titles of a peer's offer fit, admitting each title whole or not at all.
pub fn admit(
    offer: &[(u32, usize, usize)],
    room: impl Fn(u32) -> (usize, usize),
) -> BTreeMap<u32, (usize, usize)> {
    let mut want: BTreeMap<u32, (usize, usize)> = BTreeMap::new();
    for &(title, bytes, count) in offer {
        let w = want.entry(title).or_default();
        w.0 += bytes;
        w.1 += count;
    }
    want.retain(|&title, &mut (bytes, count)| {
        let (free_bytes, free_slots) = room(title);
        bytes <= free_bytes && count <= free_slots
    });
    want
}

impl Store {
    /// Opens or creates the store at `root`, holding it against other processes until dropped.
    pub fn open(root: &Path) -> Result<Self, Error> {
        let root = std::path::absolute(root)?;
        let cec = root.join(CEC_DIR);
        disk::create_dir(&cec)?;
        let Some(lock) = disk::try_lock(&root.join(LOCK_FILE))? else {
            return Err(Error::Busy(format!(
                "{} is in use by another process",
                root.display()
            )));
        };
        let mut store = Store {
            root,
            cec,
            titles: Vec::new(),
            state: State::default(),
            damaged: Vec::new(),
            _lock: lock,
        };
        store.recover()?;
        if !store.root.join(SEED_FILE).exists() {
            store.set_seed(&crate::identity::random_seed()?)?;
        }
        Ok(store)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Files moved aside while the store was opened.
    pub fn damaged(&self) -> &[Damaged] {
        &self.damaged
    }

    fn recover(&mut self) -> Result<(), Error> {
        disk::remove_leftovers(&self.root)?;
        let list = self.cec.join(LIST_FILE);
        match fs::read(&list) {
            Ok(data) => match parse_list(&data) {
                Ok(titles) => self.titles = titles,
                Err(why) => {
                    self.set_aside(&list, why)?;
                    self.titles = self.title_dirs()?;
                    self.titles.truncate(MAX_TITLES);
                    self.write_list()?;
                }
            },
            Err(e) if e.kind() == io::ErrorKind::NotFound => self.write_list()?,
            Err(e) => return Err(e.into()),
        }
        let state = self.root.join(STATE_FILE);
        match fs::read(&state) {
            Ok(data) => match serde_json::from_slice(&data) {
                Ok(s) => self.state = s,
                Err(e) => {
                    self.set_aside(&state, e.to_string())?;
                    self.rebuild_state()?;
                }
            },
            Err(e) if e.kind() == io::ErrorKind::NotFound => self.rebuild_state()?,
            Err(e) => return Err(e.into()),
        }
        for title in self.titles.clone() {
            let info = self.title_dir(title).join(INFO_FILE);
            let good = match fs::read(&info) {
                Ok(d) if d.len() >= INFO_KEY.end && u16_at(&d, 0) == INFO_MAGIC => true,
                Ok(d) => {
                    self.set_aside(
                        &info,
                        format!("{} bytes without the MBoxInfo magic", d.len()),
                    )?;
                    false
                }
                Err(e) if e.kind() == io::ErrorKind::NotFound => false,
                Err(e) => return Err(e.into()),
            };
            if !good {
                self.state.keyed.remove(&title);
                self.save_state()?;
                self.add_title(title, None, None, false)?;
            }
            for mailbox in Mailbox::ALL {
                let mut moved = false;
                for (path, why) in self.scan_box(title, mailbox)?.1 {
                    self.set_aside(&path, why)?;
                    moved = true;
                }
                if moved {
                    self.write_box_info(title, mailbox)?;
                }
            }
        }
        Ok(())
    }

    /// Recovers which titles hold a key from the MBoxInfo files.
    fn rebuild_state(&mut self) -> Result<(), Error> {
        self.state = State::default();
        for &title in &self.titles {
            if let Ok(d) = fs::read(self.title_dir(title).join(INFO_FILE)) {
                if d.get(INFO_KEY).is_some_and(|k| k.iter().any(|&b| b != 0)) {
                    self.state.keyed.insert(title);
                }
            }
        }
        self.save_state()
    }

    fn set_aside(&mut self, path: &Path, reason: String) -> Result<(), Error> {
        let rel = path.strip_prefix(&self.root).unwrap_or(path).to_path_buf();
        let dir = self.root.join(DAMAGED_DIR);
        disk::create_dir(&dir)?;
        let base = format!(
            "{}-{}",
            now(),
            rel.to_string_lossy().replace(['/', '\\'], "_")
        );
        let mut dest = dir.join(&base);
        let mut n = 1;
        while dest.exists() {
            dest = dir.join(format!("{base}.{n}"));
            n += 1;
        }
        fs::rename(path, &dest)?;
        self.damaged.push(Damaged {
            file: rel,
            reason,
            moved_to: dest,
        });
        Ok(())
    }

    fn title_dirs(&self) -> Result<Vec<u32>, Error> {
        let mut out = Vec::new();
        for entry in fs::read_dir(&self.cec)? {
            let entry = entry?;
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if entry.file_type()?.is_dir()
                && name.len() == 8
                && name
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            {
                out.push(u32::from_str_radix(&name, 16).unwrap());
            }
        }
        out.sort_unstable();
        Ok(out)
    }

    fn save_state(&self) -> Result<(), Error> {
        let data = serde_json::to_vec_pretty(&self.state).map_err(io::Error::other)?;
        disk::write_atomic(&self.root.join(STATE_FILE), &data)?;
        Ok(())
    }

    fn write_list(&self) -> Result<(), Error> {
        let mut b = vec![0u8; LIST_LEN];
        b[0..2].copy_from_slice(&LIST_MAGIC.to_le_bytes());
        b[4..8].copy_from_slice(&1u32.to_le_bytes());
        b[8..12].copy_from_slice(&(self.titles.len() as u32).to_le_bytes());
        for (i, t) in self.titles.iter().enumerate() {
            let at = LIST_HEADER + i * LIST_ENTRY;
            b[at..at + 8].copy_from_slice(format!("{t:08x}").as_bytes());
        }
        disk::write_atomic(&self.cec.join(LIST_FILE), &b)?;
        Ok(())
    }

    fn title_dir(&self, title: u32) -> PathBuf {
        self.cec.join(format!("{title:08x}"))
    }

    fn box_dir(&self, title: u32, mailbox: Mailbox) -> PathBuf {
        self.title_dir(title).join(mailbox.dir())
    }

    fn write_box_info(&self, title: u32, mailbox: Mailbox) -> Result<(), Error> {
        let held = self.messages(Some(title), Some(mailbox))?;
        let used: usize = held.iter().map(|s| s.container.len()).sum();
        let mut b = vec![0u8; BOX_INFO_LEN];
        b[0..2].copy_from_slice(&BOX_MAGIC.to_le_bytes());
        for (i, v) in [
            BOX_INFO_LEN,
            INBOX_MAX_BYTES,
            used,
            INBOX_MAX_MESSAGES,
            held.len(),
        ]
        .into_iter()
        .enumerate()
        {
            b[4 + i * 4..8 + i * 4].copy_from_slice(&(v as u32).to_le_bytes());
        }
        disk::write_atomic(&self.box_dir(title, mailbox).join(BOX_INFO_FILE), &b)?;
        Ok(())
    }

    pub fn titles(&self) -> &[u32] {
        &self.titles
    }

    /// Registers a title, or sets its name or key, refusing a different key unless `replace_key`.
    pub fn add_title(
        &mut self,
        title: u32,
        name: Option<&str>,
        key: Option<&TitleKey>,
        replace_key: bool,
    ) -> Result<(), Error> {
        let known = self.titles.contains(&title);
        if !known && self.titles.len() >= MAX_TITLES {
            return Err(Error::Format(format!(
                "a store holds at most {MAX_TITLES} titles"
            )));
        }
        if let (Some(key), Some(held)) = (key, self.title_key(title)) {
            if held != *key && !replace_key {
                return Err(Error::KeyConflict(title));
            }
        }
        for mailbox in Mailbox::ALL {
            disk::create_dir(&self.box_dir(title, mailbox))?;
        }
        if !known {
            self.titles.push(title);
            self.write_list()?;
        }
        let info_path = self.title_dir(title).join(INFO_FILE);
        let old = fs::read(&info_path).unwrap_or_default();
        let mut info = old.clone();
        info.resize(info.len().max(INFO_LEN), 0);
        info[0..2].copy_from_slice(&INFO_MAGIC.to_le_bytes());
        info[2..4].fill(0);
        info[4..8].copy_from_slice(&title.to_le_bytes());
        if let Some(key) = key {
            info[INFO_KEY].copy_from_slice(key);
        }
        if info != old {
            disk::write_atomic(&info_path, &info)?;
        }
        if key.is_some() && self.state.keyed.insert(title) {
            self.save_state()?;
        }
        if let Some(name) = name {
            let mut data: Vec<u8> = name.encode_utf16().flat_map(u16::to_le_bytes).collect();
            data.extend_from_slice(&[0, 0]);
            let path = self.title_dir(title).join(NAME_FILE);
            if fs::read(&path).ok().as_ref() != Some(&data) {
                disk::write_atomic(&path, &data)?;
            }
        }
        for mailbox in Mailbox::ALL {
            self.write_box_info(title, mailbox)?;
        }
        Ok(())
    }

    /// The title's signing key, if one was ever given for it.
    pub fn title_key(&self, title: u32) -> Option<TitleKey> {
        if !self.state.keyed.contains(&title) {
            return None;
        }
        let info = fs::read(self.title_dir(title).join(INFO_FILE)).ok()?;
        info.get(INFO_KEY)?.try_into().ok()
    }

    pub fn title_name(&self, title: u32) -> Option<String> {
        fs::read(self.title_dir(title).join(NAME_FILE))
            .ok()
            .map(|d| decode_name(&d))
    }

    /// Reads one box, returning its usable messages and the files that are not.
    fn scan_box(&self, title: u32, mailbox: Mailbox) -> Result<(Vec<Container>, Unusable), Error> {
        let mut good = Vec::new();
        let mut bad = Vec::new();
        let dir = self.box_dir(title, mailbox);
        let entries = match fs::read_dir(&dir) {
            Ok(e) => e,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok((good, bad)),
            Err(e) => return Err(e.into()),
        };
        let mut names: Vec<String> = entries
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with('_'))
            .collect();
        names.sort();
        for name in names {
            let path = dir.join(&name);
            let data = match fs::read(&path) {
                Ok(d) => d,
                Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
                Err(e) => return Err(e.into()),
            };
            match Container::parse(&data) {
                Err(e) => bad.push((path, e.to_string())),
                Ok(c) if c.title() != title => {
                    bad.push((path, format!("belongs to title {:08X}", c.title())))
                }
                Ok(c) if message_file_name(c.message_id()) != name => {
                    bad.push((path, "message ID does not match the file name".into()))
                }
                Ok(c) => good.push(c),
            }
        }
        Ok((good, bad))
    }

    /// Every usable message, optionally narrowed to one title or box.
    pub fn messages(
        &self,
        title: Option<u32>,
        mailbox: Option<Mailbox>,
    ) -> Result<Vec<Stored>, Error> {
        let mut out = Vec::new();
        let titles = title.map_or_else(|| self.titles.clone(), |t| vec![t]);
        for t in titles {
            for m in Mailbox::ALL {
                if mailbox.is_some_and(|want| want != m) {
                    continue;
                }
                for container in self.scan_box(t, m)?.0 {
                    out.push(Stored {
                        title: t,
                        mailbox: m,
                        container,
                    });
                }
            }
        }
        Ok(out)
    }

    fn check(&self, c: &Container, key: Option<&TitleKey>) -> Result<(), Error> {
        let held = self.title_key(c.title());
        match key.or(held.as_ref()) {
            Some(k) if !c.verify(k) => Err(Error::Authentication),
            _ => Ok(()),
        }
    }

    fn write_message(&mut self, c: &Container, mailbox: Mailbox) -> Result<PathBuf, Error> {
        let path = self
            .box_dir(c.title(), mailbox)
            .join(message_file_name(c.message_id()));
        disk::write_atomic(&path, c.as_bytes())?;
        Ok(path)
    }

    fn note_origin(
        &mut self,
        mailbox: Mailbox,
        id: u64,
        origin: Option<Origin>,
    ) -> Result<(), Error> {
        if let Some(origin) = origin {
            self.state.origins.insert(
                origin_key(mailbox, id),
                Provenance {
                    origin,
                    when: now(),
                },
            );
            self.save_state()?;
        }
        Ok(())
    }

    /// Files one message, signed under `key` or the title's own key, returning how many inbox messages were dropped to fit it.
    pub fn add_message(
        &mut self,
        c: &Container,
        mailbox: Mailbox,
        key: Option<&TitleKey>,
        origin: Option<Origin>,
    ) -> Result<usize, Error> {
        self.check(c, key)?;
        let title = c.title();
        if !self.titles.contains(&title) || key.is_some() {
            self.add_title(title, None, key, false)?;
        }
        self.write_message(c, mailbox)?;
        let dropped = if mailbox == Mailbox::Inbox {
            self.trim_inbox(title, c.message_id())?
        } else {
            0
        };
        self.write_box_info(title, mailbox)?;
        self.note_origin(mailbox, c.message_id(), origin)?;
        Ok(dropped)
    }

    /// Makes `c` the only outbox message of its title, returning how many it replaced.
    pub fn replace_outbox(
        &mut self,
        c: &Container,
        key: Option<&TitleKey>,
        origin: Option<Origin>,
    ) -> Result<usize, Error> {
        self.check(c, key)?;
        let old = self.messages(Some(c.title()), Some(Mailbox::Outbox))?;
        self.add_message(c, Mailbox::Outbox, key, origin)?;
        let mut n = 0;
        for s in old {
            let id = s.container.message_id();
            if id != c.message_id() && self.remove_message(s.title, Mailbox::Outbox, id)? {
                n += 1;
            }
        }
        Ok(n)
    }

    /// Free bytes and message slots left in a title's inbox.
    pub fn inbox_room(&self, title: u32) -> Result<(usize, usize), Error> {
        let held = self.messages(Some(title), Some(Mailbox::Inbox))?;
        let used: usize = held.iter().map(|s| s.container.len()).sum();
        Ok((
            INBOX_MAX_BYTES.saturating_sub(used),
            INBOX_MAX_MESSAGES.saturating_sub(held.len()),
        ))
    }

    /// Files one title's arrivals together without dropping anything, refusing all of them if they do not fit.
    pub fn file_inbound(&mut self, arrived: &[Container], from: Mac) -> Result<Filed, Error> {
        let mut result = Filed::default();
        let mut good = Vec::new();
        for c in arrived {
            match self.check(c, None) {
                Ok(()) => good.push(c),
                Err(_) => result.refused.push(c.message_id()),
            }
        }
        let Some(first) = good.first() else {
            return Ok(result);
        };
        let title = first.title();
        if good.iter().any(|c| c.title() != title) {
            return Err(Error::Format("arrivals for more than one title".into()));
        }
        let held: BTreeMap<u64, usize> = self
            .messages(Some(title), Some(Mailbox::Inbox))?
            .iter()
            .map(|s| (s.container.message_id(), s.container.len()))
            .collect();
        let new: BTreeMap<u64, usize> = good.iter().map(|c| (c.message_id(), c.len())).collect();
        let slots = new.keys().filter(|id| !held.contains_key(id)).count();
        let bytes: i64 = new
            .iter()
            .map(|(id, &n)| n as i64 - *held.get(id).unwrap_or(&0) as i64)
            .sum();
        let (free_bytes, free_slots) = self.inbox_room(title)?;
        if slots > free_slots || bytes > free_bytes as i64 {
            return Err(Error::NoRoom(format!(
                "{title:08X} inbox has {free_slots} slot(s) and {free_bytes} bytes free"
            )));
        }
        if !self.titles.contains(&title) {
            self.add_title(title, None, None, false)?;
        }
        for c in good {
            self.write_message(c, Mailbox::Inbox)?;
            self.state.origins.insert(
                origin_key(Mailbox::Inbox, c.message_id()),
                Provenance {
                    origin: Origin::Peer(from),
                    when: now(),
                },
            );
            result.filed.push(c.message_id());
        }
        self.save_state()?;
        self.write_box_info(title, Mailbox::Inbox)?;
        Ok(result)
    }

    /// Drops the oldest inbox messages other than `keep` until the inbox is within its limits.
    fn trim_inbox(&mut self, title: u32, keep: u64) -> Result<usize, Error> {
        let dir = self.box_dir(title, Mailbox::Inbox);
        let mut others = Vec::new();
        let mut used = 0;
        let mut count = 0;
        for c in self.scan_box(title, Mailbox::Inbox)?.0 {
            used += c.len();
            count += 1;
            if c.message_id() != keep {
                let path = dir.join(message_file_name(c.message_id()));
                let modified = fs::metadata(&path)?.modified()?;
                others.push((modified, c.message_id(), c.len()));
            }
        }
        others.sort();
        let mut dropped = 0;
        for (_, id, len) in others {
            if count <= INBOX_MAX_MESSAGES && used <= INBOX_MAX_BYTES {
                break;
            }
            self.remove_message(title, Mailbox::Inbox, id)?;
            count -= 1;
            used -= len;
            dropped += 1;
        }
        Ok(dropped)
    }

    pub fn remove_message(&mut self, title: u32, mailbox: Mailbox, id: u64) -> Result<bool, Error> {
        let path = self.box_dir(title, mailbox).join(message_file_name(id));
        match fs::remove_file(&path) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(false),
            Err(e) => return Err(e.into()),
        }
        self.write_box_info(title, mailbox)?;
        if self
            .state
            .origins
            .remove(&origin_key(mailbox, id))
            .is_some()
        {
            self.save_state()?;
        }
        Ok(true)
    }

    /// Forgets a title with its messages, name and key, returning how many messages went with it.
    pub fn remove_title(&mut self, title: u32) -> Result<usize, Error> {
        let held = self.messages(Some(title), None)?;
        for s in &held {
            self.state
                .origins
                .remove(&origin_key(s.mailbox, s.container.message_id()));
        }
        let dir = self.title_dir(title);
        if dir.exists() {
            fs::remove_dir_all(&dir)?;
        }
        self.titles.retain(|&t| t != title);
        self.write_list()?;
        self.state.keyed.remove(&title);
        self.state.receive.remove(&title);
        self.state.services.remove(&title);
        self.save_state()?;
        Ok(held.len())
    }

    /// Empties one box across every title.
    pub fn clear(&mut self, mailbox: Mailbox) -> Result<usize, Error> {
        let mut n = 0;
        for s in self.messages(None, Some(mailbox))? {
            if self.remove_message(s.title, mailbox, s.container.message_id())? {
                n += 1;
            }
        }
        Ok(n)
    }

    /// Whether a message matches its title's key, or `None` without one.
    pub fn verify(&self, c: &Container) -> Option<bool> {
        self.title_key(c.title()).map(|k| c.verify(&k))
    }

    pub fn origin(&self, mailbox: Mailbox, id: u64) -> Option<&Provenance> {
        self.state.origins.get(&origin_key(mailbox, id))
    }

    /// Titles set to accept messages from peers.
    pub fn receive_titles(&self) -> Vec<u32> {
        self.titles
            .iter()
            .copied()
            .filter(|t| self.state.receive.contains(t))
            .collect()
    }

    pub fn set_receive(&mut self, title: u32, on: bool) -> Result<(), Error> {
        if on && !self.titles.contains(&title) {
            return Err(Error::Format(format!(
                "title {title:08X} is not in the store"
            )));
        }
        let changed = if on {
            self.state.receive.insert(title)
        } else {
            self.state.receive.remove(&title)
        };
        if changed {
            self.save_state()?;
        }
        Ok(())
    }

    /// The record a title advertises when its outbox is empty.
    pub fn service(&self, title: u32) -> Option<Service> {
        let raw = self.state.services.get(&title)?;
        let bytes: Vec<u8> = (0..raw.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(raw.get(i..i + 2)?, 16).ok())
            .collect::<Option<_>>()?;
        let (mut found, err) = service::decode(&bytes);
        (err.is_none() && found.len() == 1 && found[0].title == title).then(|| found.remove(0))
    }

    pub fn set_service(&mut self, title: u32, record: Option<&Service>) -> Result<(), Error> {
        match record {
            Some(s) => {
                if s.title != title || !self.titles.contains(&title) {
                    return Err(Error::Format(format!(
                        "no record for title {title:08X} can be set"
                    )));
                }
                let mut raw = Vec::new();
                s.write(&mut raw);
                let hex = raw.iter().map(|b| format!("{b:02x}")).collect();
                self.state.services.insert(title, hex);
            }
            None => {
                if self.state.services.remove(&title).is_none() {
                    return Ok(());
                }
            }
        }
        self.save_state()
    }

    pub fn seed(&self) -> Result<Seed, Error> {
        let text = fs::read_to_string(self.root.join(SEED_FILE))?;
        let text = text.trim();
        let mut seed = [0u8; 32];
        if text.len() != 64 {
            return Err(Error::Format("seed file does not hold 32 bytes".into()));
        }
        for (i, b) in seed.iter_mut().enumerate() {
            *b = u8::from_str_radix(&text[i * 2..i * 2 + 2], 16)
                .map_err(|_| Error::Format("seed file is not hex".into()))?;
        }
        Ok(seed)
    }

    /// Replaces the seed every identity field is derived from.
    pub fn set_seed(&mut self, seed: &Seed) -> Result<(), Error> {
        let mut text: String = seed.iter().map(|b| format!("{b:02x}")).collect();
        text.push('\n');
        disk::write_atomic(&self.root.join(SEED_FILE), text.as_bytes())?;
        Ok(())
    }

    pub fn identity(&self) -> Result<crate::identity::Identity, Error> {
        Ok(crate::identity::Identity::from_seed(&self.seed()?))
    }

    pub fn peer_book(&self) -> Result<PeerBook, Error> {
        PeerBook::open(&self.root.join(PEERS_FILE))
    }
}

fn parse_list(data: &[u8]) -> Result<Vec<u32>, String> {
    if data.len() < LIST_LEN {
        return Err(format!("MBoxList is {} bytes", data.len()));
    }
    if u16_at(data, 0) != LIST_MAGIC {
        return Err("MBoxList has no magic".into());
    }
    let count = (u32_at(data, 8) as usize).min(MAX_TITLES);
    let mut out = Vec::new();
    for i in 0..count {
        let at = LIST_HEADER + i * LIST_ENTRY;
        let entry = std::str::from_utf8(&data[at..at + 8]).ok();
        if let Some(t) = entry.and_then(|s| u32::from_str_radix(s, 16).ok()) {
            if !out.contains(&t) {
                out.push(t);
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests;
