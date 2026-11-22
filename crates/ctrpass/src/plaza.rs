//! StreetPass Mii Plaza message bodies, read and edited in place so unknown bytes are kept.

use serde::Serialize;

use crate::Error;
use crate::cec::icon::Icon;
use crate::keys::Key;
use crate::mii::{self, Record, WRAPPED_SIZE};

pub const TITLE: u32 = 0x0002_0800;
/// Body size of the current Plaza; the original release used a shorter one.
pub const BODY_LEN: usize = 0x7280;
pub const OLD_BODY_LEN: usize = 0x60f0;
/// The Plaza sends and receives in the same exchange.
pub const SEND_MODE: u8 = 0;

pub const LANGUAGES: usize = 16;
/// A title ID in the application category with no title of its own.
pub const NO_FEATURED_TITLE: u64 = 0x0004_0000_0000_0000;

const FEATURED_ID: usize = 0x0000;
const REGION_LOCKOUT: usize = 0x0010;
const RATINGS: usize = 0x0014;
const RATINGS_LEN: usize = 0x10;
const NAMES: usize = 0x0030;
const NAMES_STRIDE: usize = 0x200;
const SHORT_BYTES: usize = 0x80;
const LONG_BYTES: usize = 0x100;
const PUBLISHER_BYTES: usize = 0x80;
const ICON_KIND: usize = 0x2030;
const ICON_LINK: usize = 0x2034;
const SMALL_ICON: usize = 0x2038;
const LARGE_ICON: usize = SMALL_ICON + 24 * 24 * 2;
const PRESENCE: usize = 0x36b8;
const MII_BOX: usize = 0x36bc;
const MII_BOX_HEADER: usize = 0x18;
const ADDRESS: usize = 0x375c;
const COUNTRY: usize = 0x3760;
const REGION: usize = 0x3b60;
const PLACE_BYTES: usize = 0x40;
const PROFILE: usize = 0x3f60;
const GREETING: usize = 0x3f70;
const GREETING_BYTES: usize = 0x22;
const RECENT: usize = 0x3f92;
const RECENT_NAMES: usize = 0x4032;
const RECENT_CREATORS: usize = 0x4252;
const RECENT_NAME_BYTES: usize = 0x22;
const RECENT_COUNT: usize = 16;
const CACHE_COUNT: usize = 0x4482;
const OLDER: usize = 0x4484;
const OLDER_COUNT: usize = 32;
const BUBBLE: usize = 0x614c;

/// Present word the current Plaza writes; the original release wrote 1.
pub const PRESENT: u32 = 0x100;
const MII_BOX_MAGIC: [&[u8; 4]; 2] = [b"CFPB", b"CFPN"];
/// Hat IDs the Plaza resets to none.
pub const REFUSED_HATS: [u8; 4] = [69, 75, 168, 169];
pub const MAX_HAT: u8 = 203;
pub const MAX_PET: u8 = 2;
pub const MAX_DREAM: u8 = 19;
pub const MAX_HOBBY: u8 = 27;
pub const MAX_HITS: u32 = 999_999;
pub const MAX_POPULATION: u16 = 3000;
pub const MAX_BUBBLE: u8 = 5;

fn utf16_at(b: &[u8], at: usize, bytes: usize) -> String {
    let units: Vec<u16> = b[at..at + bytes]
        .chunks(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .take_while(|&u| u != 0)
        .collect();
    String::from_utf16_lossy(&units)
}

fn put_utf16(b: &mut [u8], at: usize, bytes: usize, s: &str, what: &str) -> Result<(), Error> {
    let units: Vec<u16> = s.encode_utf16().collect();
    let max = bytes / 2 - 1;
    if units.len() > max {
        return Err(Error::Format(format!(
            "{what} {s:?} is longer than {max} UTF-16 units"
        )));
    }
    b[at..at + bytes].fill(0);
    for (i, u) in units.iter().enumerate() {
        b[at + 2 * i..at + 2 * i + 2].copy_from_slice(&u.to_le_bytes());
    }
    Ok(())
}

fn u32_at(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(b[at..at + 4].try_into().unwrap())
}

/// Text shown on the featured software, in one language.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Names {
    pub short: String,
    pub long: String,
    pub publisher: String,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Profile {
    pub hits: u32,
    pub population: u16,
    pub hat: u8,
    pub pet: u8,
    pub dream: u8,
    pub hobby: u8,
}

/// Someone the sender met, as listed in its history.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Met {
    pub mii_id: u32,
    pub mac: [u8; 6],
    pub name: Option<String>,
    pub creator: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Plaza {
    body: Vec<u8>,
}

impl Plaza {
    pub fn parse(body: &[u8]) -> Result<Self, Error> {
        match body.len() {
            BODY_LEN => Ok(Plaza {
                body: body.to_vec(),
            }),
            OLD_BODY_LEN => Err(Error::Format(
                "this Plaza message is from the original release, which is not supported".into(),
            )),
            n => Err(Error::Format(format!(
                "a Plaza body is {BODY_LEN:#x} bytes, not {n:#x}"
            ))),
        }
    }

    /// A body carrying `mii` with no featured software and empty text.
    pub fn new(record_key: &Key, mii: &Record) -> Result<Self, Error> {
        let mut p = Plaza {
            body: vec![0; BODY_LEN],
        };
        p.body[MII_BOX..MII_BOX + 4].copy_from_slice(MII_BOX_MAGIC[0]);
        p.body[MII_BOX + 4..MII_BOX + 8].copy_from_slice(&1u32.to_le_bytes());
        p.body[MII_BOX + 0x14..MII_BOX + 0x16].copy_from_slice(&1u16.to_le_bytes());
        p.set_presence(PRESENT);
        p.set_featured_id(NO_FEATURED_TITLE);
        p.set_region_lockout(0x7fff_ffff);
        p.set_icons(&Icon::standard(24), &Icon::standard(48))?;
        p.set_mii(record_key, mii)?;
        Ok(p)
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.body
    }

    fn mii_entry(&self) -> Result<&[u8], Error> {
        let magic = &self.body[MII_BOX..MII_BOX + 4];
        if !MII_BOX_MAGIC.iter().any(|m| &m[..] == magic) {
            return Err(Error::Format("the body holds no Mii".into()));
        }
        let at = MII_BOX + MII_BOX_HEADER;
        Ok(&self.body[at..at + WRAPPED_SIZE])
    }

    pub fn mii(&self, record_key: &Key) -> Result<Record, Error> {
        mii::unwrap(record_key, self.mii_entry()?)
    }

    /// Replaces the Mii, refusing one the Plaza would drop.
    pub fn set_mii(&mut self, record_key: &Key, mii: &Record) -> Result<(), Error> {
        self.mii_entry()?;
        let mii = mii.for_sending();
        if let Some(issue) = mii.validate().first() {
            return Err(Error::Format(format!(
                "the Plaza would refuse this Mii: {issue}"
            )));
        }
        let at = MII_BOX + MII_BOX_HEADER;
        self.body[at..at + WRAPPED_SIZE].copy_from_slice(&mii::wrap(record_key, &mii));
        Ok(())
    }

    pub fn greeting(&self) -> String {
        utf16_at(&self.body, GREETING, GREETING_BYTES)
    }

    pub fn set_greeting(&mut self, s: &str) -> Result<(), Error> {
        put_utf16(&mut self.body, GREETING, GREETING_BYTES, s, "greeting")
    }

    pub fn featured_id(&self) -> u64 {
        u64::from_le_bytes(self.body[FEATURED_ID..FEATURED_ID + 8].try_into().unwrap())
    }

    pub fn set_featured_id(&mut self, id: u64) {
        self.body[FEATURED_ID..FEATURED_ID + 8].copy_from_slice(&id.to_le_bytes());
    }

    pub fn region_lockout(&self) -> u32 {
        u32_at(&self.body, REGION_LOCKOUT)
    }

    pub fn set_region_lockout(&mut self, v: u32) {
        self.body[REGION_LOCKOUT..REGION_LOCKOUT + 4].copy_from_slice(&v.to_le_bytes());
    }

    pub fn ratings(&self) -> &[u8] {
        &self.body[RATINGS..RATINGS + RATINGS_LEN]
    }

    pub fn names(&self, language: usize) -> Names {
        let at = NAMES + language * NAMES_STRIDE;
        Names {
            short: utf16_at(&self.body, at, SHORT_BYTES),
            long: utf16_at(&self.body, at + SHORT_BYTES, LONG_BYTES),
            publisher: utf16_at(&self.body, at + SHORT_BYTES + LONG_BYTES, PUBLISHER_BYTES),
        }
    }

    /// Sets the featured software's text in every language.
    pub fn set_names(&mut self, names: &Names) -> Result<(), Error> {
        for i in 0..LANGUAGES {
            let at = NAMES + i * NAMES_STRIDE;
            put_utf16(&mut self.body, at, SHORT_BYTES, &names.short, "short name")?;
            put_utf16(
                &mut self.body,
                at + SHORT_BYTES,
                LONG_BYTES,
                &names.long,
                "long name",
            )?;
            put_utf16(
                &mut self.body,
                at + SHORT_BYTES + LONG_BYTES,
                PUBLISHER_BYTES,
                &names.publisher,
                "publisher",
            )?;
        }
        Ok(())
    }

    pub fn icon_kind(&self) -> u32 {
        u32_at(&self.body, ICON_KIND)
    }

    pub fn icons(&self) -> (Icon, Icon) {
        (
            Icon::decode(24, &self.body[SMALL_ICON..]).unwrap(),
            Icon::decode(48, &self.body[LARGE_ICON..]).unwrap(),
        )
    }

    pub fn set_icons(&mut self, small: &Icon, large: &Icon) -> Result<(), Error> {
        if small.size != 24 || large.size != 48 {
            return Err(Error::Format("Plaza icons are 24x24 and 48x48".into()));
        }
        self.body[SMALL_ICON..LARGE_ICON].copy_from_slice(&small.encode());
        self.body[LARGE_ICON..LARGE_ICON + Icon::byte_len(48)].copy_from_slice(&large.encode());
        // The receiver fills in its own pointer to the icons.
        self.body[ICON_LINK..ICON_LINK + 4].fill(0);
        Ok(())
    }

    pub fn presence(&self) -> u32 {
        u32_at(&self.body, PRESENCE)
    }

    pub fn set_presence(&mut self, v: u32) {
        self.body[PRESENCE..PRESENCE + 4].copy_from_slice(&v.to_le_bytes());
    }

    pub fn address(&self) -> u32 {
        u32_at(&self.body, ADDRESS)
    }

    pub fn set_address(&mut self, v: u32) {
        self.body[ADDRESS..ADDRESS + 4].copy_from_slice(&v.to_le_bytes());
    }

    /// The sender's country and region in one language.
    pub fn place(&self, language: usize) -> (String, String) {
        (
            utf16_at(&self.body, COUNTRY + language * PLACE_BYTES, PLACE_BYTES),
            utf16_at(&self.body, REGION + language * PLACE_BYTES, PLACE_BYTES),
        )
    }

    pub fn set_place(&mut self, country: &str, region: &str) -> Result<(), Error> {
        for i in 0..LANGUAGES {
            put_utf16(
                &mut self.body,
                COUNTRY + i * PLACE_BYTES,
                PLACE_BYTES,
                country,
                "country",
            )?;
            put_utf16(
                &mut self.body,
                REGION + i * PLACE_BYTES,
                PLACE_BYTES,
                region,
                "region",
            )?;
        }
        Ok(())
    }

    pub fn profile(&self) -> Profile {
        let b = &self.body[PROFILE..];
        Profile {
            hits: u32_at(b, 0),
            population: u16::from_le_bytes([b[4], b[5]]),
            hat: b[6],
            pet: b[7],
            dream: b[8],
            hobby: b[9],
        }
    }

    pub fn set_profile(&mut self, p: &Profile) {
        let b = &mut self.body[PROFILE..PROFILE + 10];
        b[..4].copy_from_slice(&p.hits.to_le_bytes());
        b[4..6].copy_from_slice(&p.population.to_le_bytes());
        b[6..].copy_from_slice(&[p.hat, p.pet, p.dream, p.hobby]);
    }

    pub fn bubble(&self) -> u8 {
        self.body[BUBBLE]
    }

    pub fn set_bubble(&mut self, v: u8) {
        self.body[BUBBLE] = v;
    }

    fn met_at(&self, at: usize) -> (u32, [u8; 6]) {
        (
            u32_at(&self.body, at),
            self.body[at + 4..at + 10].try_into().unwrap(),
        )
    }

    /// Everyone in the sender's history that is set, most recent first.
    pub fn history(&self) -> Vec<Met> {
        let mut out = Vec::new();
        for i in 0..RECENT_COUNT {
            let (mii_id, mac) = self.met_at(RECENT + i * 10);
            if mii_id != 0 {
                out.push(Met {
                    mii_id,
                    mac,
                    name: Some(utf16_at(
                        &self.body,
                        RECENT_NAMES + i * RECENT_NAME_BYTES,
                        RECENT_NAME_BYTES,
                    )),
                    creator: Some(utf16_at(
                        &self.body,
                        RECENT_CREATORS + i * RECENT_NAME_BYTES,
                        RECENT_NAME_BYTES,
                    )),
                });
            }
        }
        for i in 0..OLDER_COUNT {
            let (mii_id, mac) = self.met_at(OLDER + i * 10);
            if mii_id != 0 {
                out.push(Met {
                    mii_id,
                    mac,
                    name: None,
                    creator: None,
                });
            }
        }
        out
    }

    pub fn cache_count(&self) -> u8 {
        self.body[CACHE_COUNT]
    }

    /// Every value a receiving Plaza would refuse or reset.
    pub fn problems(&self) -> Vec<String> {
        let mut out = Vec::new();
        let id = self.featured_id();
        if id == 0 || id == u64::MAX {
            out.push(format!("featured title ID {id:016x} is refused"));
        }
        if self.icon_kind() > 2 {
            out.push(format!("icon kind {} is above 2", self.icon_kind()));
        }
        if self.mii_entry().is_err() {
            out.push("the body holds no Mii".into());
        }
        let p = self.profile();
        let mut over = |what: &str, v: u32, max: u32| {
            if v > max {
                out.push(format!("{what} {v} is above {max}"));
            }
        };
        over("hits", p.hits, MAX_HITS);
        over("population", p.population.into(), MAX_POPULATION.into());
        over("hat", p.hat.into(), MAX_HAT.into());
        over("pet", p.pet.into(), MAX_PET.into());
        over("dream", p.dream.into(), MAX_DREAM.into());
        over("hobby", p.hobby.into(), MAX_HOBBY.into());
        over("speech bubble", self.bubble().into(), MAX_BUBBLE.into());
        if REFUSED_HATS.contains(&p.hat) {
            out.push(format!("hat {} is reset to none", p.hat));
        }
        out
    }
}

mod title;
pub use title::PlazaTitle;

#[cfg(test)]
mod tests;
