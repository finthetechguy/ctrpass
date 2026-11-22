//! Delivering messages into a console-shaped CEC folder, such as an emulator's, as if they had arrived.

use std::fs;
use std::path::{Path, PathBuf};

use crate::Error;
use crate::cec::container::{Container, HEADER_LEN, Timestamp};
use crate::keys::TitleKey;
use crate::store::{Mailbox, disk, message_file_name};

const LIST_FILE: &str = "MBoxList____";
const INFO_FILE: &str = "MBoxInfo____";
const BOX_INFO_FILE: &str = "BoxInfo_____";
const BOX_MAGIC: &[u8; 2] = b"bb";
const BOX_HEADER: usize = 0x20;
const LIST_MAGIC: u16 = 0x6868;
const LIST_HEADER: usize = 12;
const LIST_ENTRY: usize = 16;
/// Where the CEC folder sits inside an emulator's data folder, below `nand/data/<id>`.
pub const EMULATOR_SUBPATH: &str = "sysdata/00010026/00000000/CEC";

const UNOPENED: usize = 0x36;
const NEW: usize = 0x37;
const RECEIVED: usize = 0x48;
const OPENED: usize = 0x54;
const INFO_FLAGS: usize = 0x40;
const INFO_RECEIVED: usize = 0x44;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Delivery {
    Added,
    /// Added, but the folder holds no key for the title, so its signature went unchecked.
    Unverified,
    Duplicate,
    /// The title has no inbox there; its game creates one when StreetPass is turned on for it.
    NoBox,
    Full,
    TooBig,
    /// Its signature does not match the title's key there.
    Invalid,
}

impl Delivery {
    pub fn delivered(self) -> bool {
        matches!(self, Delivery::Added | Delivery::Unverified)
    }

    pub fn describe(self) -> &'static str {
        match self {
            Delivery::Added => "delivered",
            Delivery::Unverified => "delivered without a key there to check it",
            Delivery::Duplicate => "already there",
            Delivery::NoBox => "no inbox for the title there; turn StreetPass on in its game first",
            Delivery::Full => "that inbox is full",
            Delivery::TooBig => "larger than that inbox takes",
            Delivery::Invalid => "its signature does not match the title's key there",
        }
    }
}

fn u32_at(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(b[at..at + 4].try_into().unwrap())
}

/// A box's limits and the headers of the messages it lists, newest first.
struct BoxInfo {
    fields: [u32; 7],
    headers: Vec<Vec<u8>>,
}

const MAX_BYTES: usize = 1;
const MAX_MESSAGES: usize = 3;
const MAX_MESSAGE_SIZE: usize = 6;

impl BoxInfo {
    fn parse(data: &[u8]) -> Result<Self, Error> {
        if data.len() < BOX_HEADER || &data[..2] != BOX_MAGIC {
            return Err(Error::Format("not a BoxInfo file".into()));
        }
        let fields: [u32; 7] = std::array::from_fn(|i| u32_at(data, 4 + 4 * i));
        let n = fields[4] as usize;
        if BOX_HEADER + n * HEADER_LEN > data.len() {
            return Err(Error::Format(format!(
                "BoxInfo lists {n} messages it does not hold"
            )));
        }
        let headers = (0..n)
            .map(|i| data[BOX_HEADER + i * HEADER_LEN..BOX_HEADER + (i + 1) * HEADER_LEN].to_vec())
            .collect();
        Ok(BoxInfo { fields, headers })
    }

    fn used(&self) -> usize {
        self.headers.iter().map(|h| u32_at(h, 4) as usize).sum()
    }

    fn to_bytes(&self) -> Vec<u8> {
        let mut f = self.fields;
        f[0] = (BOX_HEADER + self.headers.len() * HEADER_LEN) as u32;
        f[2] = self.used() as u32;
        f[4] = self.headers.len() as u32;
        let mut b = vec![0u8; BOX_HEADER];
        b[..2].copy_from_slice(BOX_MAGIC);
        for (i, v) in f.iter().enumerate() {
            b[4 + 4 * i..8 + 4 * i].copy_from_slice(&v.to_le_bytes());
        }
        for h in &self.headers {
            b.extend_from_slice(h);
        }
        b
    }
}

/// The message as a receiving console files it: marked new and unopened, with arrival times.
pub fn as_received(c: &Container, when: Timestamp) -> Vec<u8> {
    let mut b = c.as_bytes().to_vec();
    b[UNOPENED] = 1;
    b[NEW] = 1;
    for at in [RECEIVED, OPENED] {
        if b[at..at + Timestamp::LEN].iter().all(|&x| x == 0) {
            b[at..at + Timestamp::LEN].copy_from_slice(&when.to_bytes());
        }
    }
    b
}

pub struct CecFolder {
    path: PathBuf,
}

impl CecFolder {
    /// Opens the CEC folder at `path`, its `CEC` child, or the one inside an emulator's data folder.
    pub fn open(path: &Path) -> Result<Self, Error> {
        let mut candidates = vec![path.to_path_buf(), path.join("CEC")];
        if let Ok(ids) = fs::read_dir(path.join("nand/data")) {
            let mut ids: Vec<_> = ids.filter_map(Result::ok).map(|e| e.path()).collect();
            ids.sort();
            candidates.extend(ids.into_iter().map(|d| d.join(EMULATOR_SUBPATH)));
        }
        candidates
            .into_iter()
            .find(|c| c.join(LIST_FILE).is_file())
            .map(|path| CecFolder { path })
            .ok_or_else(|| {
                Error::Format(format!(
                    "{} holds no CEC folder (no {LIST_FILE} in it, its CEC, or nand/data/*/{EMULATOR_SUBPATH})",
                    path.display()
                ))
            })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The titles the folder's list names.
    pub fn titles(&self) -> Result<Vec<u32>, Error> {
        let data = fs::read(self.path.join(LIST_FILE))?;
        if data.len() < LIST_HEADER || u16::from_le_bytes([data[0], data[1]]) != LIST_MAGIC {
            return Err(Error::Format("MBoxList has no magic".into()));
        }
        let count = u32_at(&data, 8) as usize;
        Ok((0..count)
            .filter_map(|i| {
                let at = LIST_HEADER + i * LIST_ENTRY;
                let s = std::str::from_utf8(data.get(at..at + 8)?).ok()?;
                u32::from_str_radix(s, 16).ok()
            })
            .collect())
    }

    fn title_dir(&self, title: u32) -> Option<PathBuf> {
        [format!("{title:08x}"), format!("{title:08X}")]
            .into_iter()
            .map(|n| self.path.join(n))
            .find(|d| d.is_dir())
    }

    fn inbox(&self, title: u32) -> Result<Option<PathBuf>, Error> {
        if !self.titles()?.contains(&title) {
            return Ok(None);
        }
        Ok(self
            .title_dir(title)
            .map(|d| d.join(Mailbox::Inbox.dir()))
            .filter(|b| b.join(BOX_INFO_FILE).is_file()))
    }

    pub fn title_key(&self, title: u32) -> Option<TitleKey> {
        let info = fs::read(self.title_dir(title)?.join(INFO_FILE)).ok()?;
        let key: TitleKey = info.get(0x10..0x30)?.try_into().ok()?;
        key.iter().any(|&b| b != 0).then_some(key)
    }

    /// Files one message in its title's inbox, newest first in the box's list.
    pub fn deliver(
        &self,
        c: &Container,
        when: Timestamp,
        dry_run: bool,
    ) -> Result<Delivery, Error> {
        let Some(inbox) = self.inbox(c.title())? else {
            return Ok(Delivery::NoBox);
        };
        let key = self.title_key(c.title());
        if key.is_some_and(|k| !c.verify(&k)) {
            return Ok(Delivery::Invalid);
        }
        let info_path = inbox.join(BOX_INFO_FILE);
        let mut info = BoxInfo::parse(&fs::read(&info_path)?)?;
        let name = message_file_name(c.message_id());
        let id = c.message_id().to_le_bytes();
        if info.headers.iter().any(|h| h[0x20..0x28] == id) || inbox.join(&name).exists() {
            return Ok(Delivery::Duplicate);
        }
        let max_size = info.fields[MAX_MESSAGE_SIZE] as usize;
        if max_size != 0 && c.len() > max_size {
            return Ok(Delivery::TooBig);
        }
        if info.headers.len() >= info.fields[MAX_MESSAGES] as usize
            || info.used() + c.len() > info.fields[MAX_BYTES] as usize
        {
            return Ok(Delivery::Full);
        }
        let result = if key.is_some() {
            Delivery::Added
        } else {
            Delivery::Unverified
        };
        if dry_run {
            return Ok(result);
        }
        let filed = as_received(c, when);
        disk::write_atomic(&inbox.join(&name), &filed)?;
        info.headers.insert(0, filed[..HEADER_LEN].to_vec());
        disk::write_atomic(&info_path, &info.to_bytes())?;
        self.mark_received(c.title(), &filed[OPENED..OPENED + Timestamp::LEN])?;
        Ok(result)
    }

    /// Raises the title's arrival flags so its game notices new messages.
    fn mark_received(&self, title: u32, when: &[u8]) -> Result<(), Error> {
        let Some(path) = self.title_dir(title).map(|d| d.join(INFO_FILE)) else {
            return Ok(());
        };
        let Ok(mut info) = fs::read(&path) else {
            return Ok(());
        };
        if info.len() < INFO_RECEIVED + Timestamp::LEN {
            return Ok(());
        }
        info[INFO_FLAGS] = 1;
        info[INFO_FLAGS + 1] = 1;
        info[INFO_RECEIVED..INFO_RECEIVED + Timestamp::LEN].copy_from_slice(when);
        disk::write_atomic(&path, &info)?;
        Ok(())
    }
}

/// Writes each message unchanged as `<dest>/<TITLE>/_<id>`, returning the paths written.
pub fn write_raw(messages: &[Container], dest: &Path) -> Result<Vec<PathBuf>, Error> {
    let mut out = Vec::new();
    for c in messages {
        let dir = dest.join(format!("{:08X}", c.title()));
        disk::create_dir(&dir)?;
        let path = dir.join(message_file_name(c.message_id()));
        disk::write_atomic(&path, c.as_bytes())?;
        out.push(path);
    }
    Ok(out)
}
