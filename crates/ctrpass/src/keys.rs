//! Loads keys from a `name=hex` key file without ever formatting or logging them.

use std::collections::BTreeMap;
use std::fmt;
use std::path::Path;

use crate::Error;

pub type Key = [u8; 16];
pub type HmacKey = [u8; 17];
pub type TitleKey = [u8; 32];

const SCRAMBLER: u128 = 0x1FF9_E9AA_C5FE_0408_0245_91DC_5D52_768A;

const LINK_SLOT: u8 = 0x2e;
const RECORD_SLOT: u8 = 0x31;
const HMAC_NAME: &str = "streetpassHmacKey";
const TITLE_PREFIX: &str = "titleKey";

/// Derives a normal key from a key pair the way the console's key scrambler does.
pub fn scramble(key_x: &Key, key_y: &Key) -> Key {
    let x = u128::from_be_bytes(*key_x);
    let y = u128::from_be_bytes(*key_y);
    (x.rotate_left(2) ^ y)
        .wrapping_add(SCRAMBLER)
        .rotate_left(87)
        .to_be_bytes()
}

/// The keys ctrpass needs.
#[derive(Clone)]
pub struct Keys {
    /// Normal key of the slot the link key is encrypted under.
    pub link: Key,
    /// HMAC key the link key is derived with, including its trailing NUL.
    pub hmac: HmacKey,
    /// Key that protects Mii records, needed only to read or build Miis.
    pub record: Option<Key>,
    /// Message signing keys by CEC title ID.
    pub titles: BTreeMap<u32, TitleKey>,
}

impl fmt::Debug for Keys {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Keys")
            .field("record", &self.record.is_some())
            .field("titles", &self.titles.keys().collect::<Vec<_>>())
            .finish_non_exhaustive()
    }
}

impl Keys {
    pub fn load(path: &Path) -> Result<Self, Error> {
        let text = std::fs::read_to_string(path).map_err(|e| Error::Keys(e.to_string()))?;
        Self::parse(&text)
    }

    pub fn parse(text: &str) -> Result<Self, Error> {
        let entries: Vec<(&str, &str)> = text
            .lines()
            .filter_map(|l| l.split_once('='))
            .map(|(k, v)| (k.trim(), v.trim()))
            .collect();
        let get = |name: &str, len: usize| -> Result<Option<Vec<u8>>, Error> {
            match entries.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)) {
                Some((_, v)) => parse_hex(v)
                    .filter(|b| b.len() == len)
                    .map(Some)
                    .ok_or_else(|| Error::Keys(format!("{name} is not {len} hex bytes"))),
                None => Ok(None),
            }
        };
        let slot = |n: u8| -> Result<Option<Key>, Error> {
            let half = |h: char| get(&format!("slot0x{n:02X}Key{h}"), 16);
            let key = |v: Vec<u8>| -> Key { v.try_into().unwrap() };
            Ok(match (half('N')?, half('X')?, half('Y')?) {
                (Some(n), _, _) => Some(key(n)),
                (None, Some(x), Some(y)) => Some(scramble(&key(x), &key(y))),
                _ => None,
            })
        };

        let link = slot(LINK_SLOT)?.ok_or_else(|| {
            Error::Keys(format!(
                "slot0x{LINK_SLOT:02X}KeyN or slot0x{LINK_SLOT:02X}KeyX/KeyY missing"
            ))
        })?;
        let hmac =
            get(HMAC_NAME, 16)?.ok_or_else(|| Error::Keys(format!("{HMAC_NAME} missing")))?;
        let mut hmac_key = [0u8; 17];
        hmac_key[..16].copy_from_slice(&hmac);

        let mut titles = BTreeMap::new();
        for (k, _) in &entries {
            let Some(id) = k
                .get(..TITLE_PREFIX.len())
                .filter(|p| p.eq_ignore_ascii_case(TITLE_PREFIX))
                .and_then(|_| u32::from_str_radix(&k[TITLE_PREFIX.len()..], 16).ok())
            else {
                continue;
            };
            let key = get(k, 32)?.unwrap();
            titles.insert(id, key.try_into().unwrap());
        }

        Ok(Keys {
            link,
            hmac: hmac_key,
            record: slot(RECORD_SLOT)?,
            titles,
        })
    }

    pub fn title(&self, title_id: u32) -> Option<&TitleKey> {
        self.titles.get(&title_id)
    }
}

fn parse_hex(hex: &str) -> Option<Vec<u8>> {
    if hex.len() % 2 != 0 || !hex.is_ascii() {
        return None;
    }
    (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASE: &str = "slot0x2EKeyN=000102030405060708090a0b0c0d0e0f\n\
                        streetpassHmacKey=41414141414141414141414141414141\n";

    #[test]
    fn scramble_is_rotations_and_add() {
        let z = [0u8; 16];
        assert_eq!(scramble(&z, &z), SCRAMBLER.rotate_left(87).to_be_bytes());
    }

    #[test]
    fn parse_base_keys() {
        let k = Keys::parse(BASE).unwrap();
        assert_eq!(k.link[15], 0x0f);
        assert_eq!(&k.hmac[..16], &[0x41; 16]);
        assert_eq!(k.hmac[16], 0);
        assert!(k.record.is_none() && k.titles.is_empty());
    }

    #[test]
    fn parse_pairs_and_titles() {
        let text = "slot0x2EKeyX=00000000000000000000000000000000\n\
                    slot0x2EKeyY=00000000000000000000000000000000\n\
                    STREETPASSHMACKEY=41414141414141414141414141414141\n\
                    slot0x31KeyN=ffffffffffffffffffffffffffffffff\n\
                    titleKey00020800=0101010101010101010101010101010101010101010101010101010101010101\n";
        let k = Keys::parse(text).unwrap();
        assert_eq!(k.link, scramble(&[0; 16], &[0; 16]));
        assert_eq!(k.record, Some([0xff; 16]));
        assert_eq!(k.title(0x0002_0800), Some(&[1u8; 32]));
    }

    #[test]
    fn parse_reports_missing_and_malformed() {
        assert!(Keys::parse("streetpassHmacKey=41414141414141414141414141414141").is_err());
        assert!(Keys::parse("slot0x2EKeyN=000102030405060708090a0b0c0d0e0f").is_err());
        assert!(Keys::parse(&format!("{BASE}slot0x31KeyN=00")).is_err());
        assert!(Keys::parse(&format!("{BASE}titleKey00020800=00")).is_err());
    }

    #[test]
    fn debug_hides_keys() {
        let s = format!("{:?}", Keys::parse(BASE).unwrap());
        assert!(!s.contains("65") && !s.contains("15"));
    }
}
