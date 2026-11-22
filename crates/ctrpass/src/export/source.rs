//! Messages and title details found in a dump: a save image, a folder, or a message file.

use std::fs;
use std::path::Path;

use super::savefs::{self, SaveFs};
use crate::Error;
use crate::cec::container::{self, Container};
use crate::cec::envelope;
use crate::keys::TitleKey;
use crate::store::Mailbox;

/// Largest file read from a dump.
const FILE_LIMIT: usize = 4 << 20;
/// Most files looked at in one folder tree.
const FILE_COUNT_LIMIT: usize = 20_000;
const DEPTH_LIMIT: usize = 16;

const INFO_FILE: &str = "MBoxInfo____";
const NAME_FILE: &str = "MBoxData.010";
const INFO_MAGIC: &[u8; 2] = b"cc";

#[derive(Clone, Debug)]
pub struct Found {
    pub container: Container,
    /// The box it was kept in, when the dump says.
    pub mailbox: Option<Mailbox>,
    /// Where in the dump it was found.
    pub path: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TitleInfo {
    pub title: u32,
    pub name: Option<String>,
    pub key: Option<TitleKey>,
}

#[derive(Clone, Debug, Default)]
pub struct Source {
    pub messages: Vec<Found>,
    pub titles: Vec<TitleInfo>,
    /// Files that looked like messages but could not be read, with the reason.
    pub skipped: Vec<(String, String)>,
}

fn decode_name(data: &[u8]) -> String {
    let units: Vec<u16> = data
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .take_while(|&u| u != 0)
        .collect();
    String::from_utf16_lossy(&units)
}

/// Reads the messages a file holds, allowing padding after a single container.
pub fn messages_in(data: &[u8]) -> Result<Vec<Container>, Error> {
    match envelope::containers(data) {
        Ok(cs) => Ok(cs),
        Err(e) => {
            let total = data
                .get(4..8)
                .map(|b| u32::from_le_bytes(b.try_into().unwrap()) as usize);
            match total {
                Some(t) if data.get(..2) == Some(&container::MAGIC[..]) && t < data.len() => {
                    Ok(vec![Container::parse(&data[..t])?])
                }
                _ => Err(e),
            }
        }
    }
}

fn mailbox_of(parts: &[&str]) -> Option<Mailbox> {
    Mailbox::ALL
        .into_iter()
        .find(|m| parts.iter().any(|p| *p == m.dir()))
}

/// The title a path inside a CEC folder belongs to, from the folder after `CEC`.
fn title_of(parts: &[&str]) -> Option<u32> {
    let at = parts.iter().position(|p| *p == "CEC")?;
    let t = parts.get(at + 1)?;
    (t.len() == 8).then(|| u32::from_str_radix(t, 16).ok())?
}

impl Source {
    fn title_mut(&mut self, title: u32) -> &mut TitleInfo {
        if let Some(i) = self.titles.iter().position(|t| t.title == title) {
            return &mut self.titles[i];
        }
        self.titles.push(TitleInfo {
            title,
            ..TitleInfo::default()
        });
        self.titles.last_mut().unwrap()
    }

    /// Takes one file found at `path`, which uses `/` between parts.
    fn take(&mut self, path: &str, data: &[u8]) {
        let parts: Vec<&str> = path.split('/').collect();
        let name = parts.last().copied().unwrap_or_default();
        let title = title_of(&parts);
        match (name, title) {
            (INFO_FILE, Some(t)) => {
                if data.get(..2) == Some(&INFO_MAGIC[..]) {
                    let key: Option<TitleKey> =
                        data.get(0x10..0x30).and_then(|k| k.try_into().ok());
                    self.title_mut(t).key = key.filter(|k| k.iter().any(|&b| b != 0));
                }
            }
            (NAME_FILE, Some(t)) => {
                let n = decode_name(data);
                if !n.is_empty() {
                    self.title_mut(t).name = Some(n);
                }
            }
            _ => {
                let looks = name.starts_with('_')
                    || data
                        .get(..2)
                        .is_some_and(|m| m == container::MAGIC || m == envelope::MAGIC);
                if !looks {
                    return;
                }
                match messages_in(data) {
                    Ok(cs) => {
                        for c in cs {
                            self.title_mut(c.title());
                            self.messages.push(Found {
                                container: c,
                                mailbox: mailbox_of(&parts),
                                path: path.into(),
                            });
                        }
                    }
                    Err(e) if name.starts_with('_') => {
                        self.skipped.push((path.into(), e.to_string()))
                    }
                    Err(_) => {}
                }
            }
        }
    }
}

pub fn read_save(data: &[u8]) -> Result<Source, Error> {
    let fs = SaveFs::open(data)?;
    let mut src = Source::default();
    for f in fs.files() {
        match fs.read(&f, FILE_LIMIT) {
            Ok(d) => src.take(&f.path, &d),
            Err(e) => src.skipped.push((f.path.clone(), e.to_string())),
        }
    }
    Ok(src)
}

fn walk(
    dir: &Path,
    rel: &str,
    depth: usize,
    seen: &mut usize,
    src: &mut Source,
) -> Result<(), Error> {
    if depth > DEPTH_LIMIT {
        return Ok(());
    }
    let mut entries: Vec<_> = fs::read_dir(dir)?.filter_map(Result::ok).collect();
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        let name = e.file_name().to_string_lossy().into_owned();
        let path = if rel.is_empty() {
            name.clone()
        } else {
            format!("{rel}/{name}")
        };
        let kind = e.file_type()?;
        if kind.is_dir() {
            walk(&e.path(), &path, depth + 1, seen, src)?;
        } else if kind.is_file() {
            *seen += 1;
            if *seen > FILE_COUNT_LIMIT {
                return Err(Error::Format(format!(
                    "more than {FILE_COUNT_LIMIT} files; point at a smaller folder"
                )));
            }
            if e.metadata()?.len() as usize <= FILE_LIMIT {
                src.take(&path, &fs::read(e.path())?);
            }
        }
    }
    Ok(())
}

/// Reads a save image, a folder tree (console-shaped or not), or a message file.
pub fn read(path: &Path) -> Result<Source, Error> {
    let meta = fs::metadata(path)?;
    if meta.is_dir() {
        let mut src = Source::default();
        // Name the root CEC when it is a console's CEC folder, so titles are found below it.
        let base = if path.join("MBoxList____").is_file() {
            "CEC"
        } else {
            ""
        };
        walk(path, base, 0, &mut 0, &mut src)?;
        return Ok(src);
    }
    if meta.len() as usize > 256 << 20 {
        return Err(Error::Format(format!(
            "{} is too large to read",
            path.display()
        )));
    }
    let data = fs::read(path)?;
    if data.starts_with(savefs::MAGIC) {
        return read_save(&data);
    }
    let mut src = Source::default();
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    for c in messages_in(&data)? {
        src.title_mut(c.title());
        src.messages.push(Found {
            container: c,
            mailbox: None,
            path: name.clone(),
        });
    }
    Ok(src)
}
