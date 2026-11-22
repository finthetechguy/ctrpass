//! Titles whose message bodies ctrpass can create and edit, described by their fields.
//!
//! Each title states its fields once, which gives it validation, text and JSON input, and a
//! form in any front end without code of its own.

use serde::Serialize;
use serde_json::{Map, Value};

use crate::Error;
use crate::cec::Container;
use crate::cec::container::{EXT_ICON, EXT_INFO_TEXT, ICON_LEN, Timestamp};
use crate::cec::icon::Icon;
use crate::identity::Identity;
use crate::keys::{Key, TitleKey};

/// Field values by key. Images are hex of their tiled bytes, byte strings are hex.
pub type Values = Map<String, Value>;

/// Names of the 16 language slots, in order.
pub const LANGUAGES: [&str; 16] = [
    "Japanese",
    "English",
    "French",
    "German",
    "Italian",
    "Spanish",
    "Chinese (simplified)",
    "Korean",
    "Dutch",
    "Portuguese",
    "Russian",
    "Chinese (traditional)",
    "unused 12",
    "unused 13",
    "unused 14",
    "unused 15",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Kind {
    /// Text of at most `max` UTF-16 units, without the characters in `forbid`.
    Text { max: usize, forbid: &'static str },
    /// A whole number shown in decimal, or hex when `hex` is set.
    Number {
        min: u64,
        max: u64,
        exclude: &'static [u64],
        hex: bool,
    },
    /// A square image of `size` pixels.
    Image { size: usize },
    /// Exactly `len` bytes.
    Bytes { len: usize },
}

#[derive(Clone, Copy, Debug, Serialize)]
pub struct Field {
    pub key: &'static str,
    pub label: &'static str,
    pub kind: Kind,
    pub help: &'static str,
    /// Rarely changed, so front ends may tuck it away.
    pub advanced: bool,
}

/// What a title may need besides the body: the Mii key and, for new bodies, an identity.
#[derive(Clone, Copy, Debug, Default)]
pub struct Context<'a> {
    pub record_key: Option<&'a Key>,
    pub identity: Option<&'a Identity>,
}

impl Context<'_> {
    pub fn record_key(&self) -> Result<&Key, Error> {
        self.record_key
            .ok_or_else(|| Error::Keys("Miis need the slot0x31 key".into()))
    }
}

pub trait Title: Sync {
    fn id(&self) -> u32;
    fn name(&self) -> &'static str;
    fn send_mode(&self) -> u8;
    fn fields(&self) -> &'static [Field];
    /// A new body, made from the context's identity.
    fn create(&self, ctx: &Context) -> Result<Vec<u8>, Error>;
    /// Every field's value in `body`.
    fn read(&self, body: &[u8], ctx: &Context) -> Result<Values, Error>;
    /// `body` with the given values applied; fields not in `values` keep their bytes.
    fn write(&self, body: &[u8], values: &Values, ctx: &Context) -> Result<Vec<u8>, Error>;
    /// Values in `body` the game would refuse or reset.
    fn problems(&self, _body: &[u8]) -> Vec<String> {
        Vec::new()
    }
    /// Text shown with a new message.
    fn message_text(&self) -> &'static str;
    /// Flags a new message carries.
    fn message_flags(&self) -> u8 {
        3
    }
}

static TITLES: &[&dyn Title] = &[&crate::plaza::PlazaTitle];

pub fn all() -> &'static [&'static dyn Title] {
    TITLES
}

/// Finds a title by hex ID or by name, ignoring case.
pub fn find(id_or_name: &str) -> Option<&'static dyn Title> {
    let id = u32::from_str_radix(id_or_name.trim_start_matches("0x"), 16).ok();
    TITLES
        .iter()
        .copied()
        .find(|t| Some(t.id()) == id || t.name().eq_ignore_ascii_case(id_or_name))
}

pub fn by_id(id: u32) -> Option<&'static dyn Title> {
    TITLES.iter().copied().find(|t| t.id() == id)
}

fn invalid(field: &Field, why: String) -> Error {
    Error::Format(format!("{}: {why}", field.key))
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn unhex(s: &str) -> Option<Vec<u8>> {
    if s.len() % 2 != 0 {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok())
        .collect()
}

/// Checks a value against its field, returning it in canonical form.
pub fn check(field: &Field, v: &Value) -> Result<Value, Error> {
    match field.kind {
        Kind::Text { max, forbid } => {
            let s = v
                .as_str()
                .ok_or_else(|| invalid(field, format!("expected text, got {v}")))?;
            let units = s.encode_utf16().count();
            if units > max {
                return Err(invalid(field, format!("{units} characters, at most {max}")));
            }
            if let Some(c) = s.chars().find(|c| forbid.contains(*c)) {
                return Err(invalid(field, format!("may not contain {c:?}")));
            }
            Ok(Value::from(s))
        }
        Kind::Number {
            min, max, exclude, ..
        } => {
            let n = v
                .as_u64()
                .ok_or_else(|| invalid(field, format!("expected a whole number, got {v}")))?;
            if n < min || n > max {
                return Err(invalid(field, format!("{n} is outside {min}..={max}")));
            }
            if exclude.contains(&n) {
                return Err(invalid(field, format!("{n} is refused")));
            }
            Ok(Value::from(n))
        }
        Kind::Image { size } => {
            let ok = v
                .as_str()
                .and_then(unhex)
                .is_some_and(|b| b.len() == Icon::byte_len(size));
            if !ok {
                return Err(invalid(field, format!("expected a {size}x{size} image")));
            }
            Ok(v.clone())
        }
        Kind::Bytes { len } => {
            let ok = v.as_str().and_then(unhex).is_some_and(|b| b.len() == len);
            if !ok {
                return Err(invalid(field, format!("expected {len} bytes as hex")));
            }
            Ok(v.clone())
        }
    }
}

/// Reads a value for `field` from command-line text.
pub fn from_text(field: &Field, text: &str) -> Result<Value, Error> {
    let v = match field.kind {
        Kind::Text { .. } | Kind::Image { .. } | Kind::Bytes { .. } => Value::from(text),
        Kind::Number { hex, .. } => {
            let t = text.trim();
            let parsed = match t.strip_prefix("0x") {
                Some(h) => u64::from_str_radix(h, 16),
                None if hex => u64::from_str_radix(t, 16),
                None => t.parse(),
            };
            Value::from(parsed.map_err(|_| invalid(field, format!("{text:?} is not a number")))?)
        }
    };
    check(field, &v)
}

/// Looks up `key` in a title's fields.
pub fn field(title: &dyn Title, key: &str) -> Result<&'static Field, Error> {
    title
        .fields()
        .iter()
        .find(|f| f.key == key)
        .ok_or_else(|| Error::Format(format!("{} has no field {key:?}", title.name())))
}

/// Checks every value in `values` against `title`'s fields.
pub fn check_all(title: &dyn Title, values: &Values) -> Result<Values, Error> {
    values
        .iter()
        .map(|(k, v)| Ok((k.clone(), check(field(title, k)?, v)?)))
        .collect()
}

/// Typed access to checked values while writing a body.
pub struct Get<'a>(pub &'a Values);

impl Get<'_> {
    pub fn text(&self, key: &str) -> Option<&str> {
        self.0.get(key).and_then(Value::as_str)
    }

    pub fn number(&self, key: &str) -> Option<u64> {
        self.0.get(key).and_then(Value::as_u64)
    }

    pub fn bytes(&self, key: &str) -> Option<Vec<u8>> {
        self.text(key).and_then(unhex)
    }

    pub fn image(&self, key: &str, size: usize) -> Option<Icon> {
        self.bytes(key).and_then(|b| Icon::decode(size, &b).ok())
    }
}

fn random_id() -> u64 {
    let mut b = [0u8; 8];
    getrandom::fill(&mut b).expect("the system random source failed");
    u64::from_le_bytes(b)
}

fn utf16z(raw: &[u8]) -> String {
    let units: Vec<u16> = raw
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .take_while(|&u| u != 0)
        .collect();
    String::from_utf16_lossy(&units)
}

/// The text a message shows, if it has any.
pub fn message_text(c: &Container) -> Option<String> {
    c.ext_header(EXT_INFO_TEXT).map(utf16z)
}

/// Signs `body` as a new message for `title` with a fresh ID.
///
/// The icon and text default to `template`'s, then to the title's own.
pub fn message(
    title: &dyn Title,
    body: &[u8],
    key: &TitleKey,
    template: Option<&Container>,
    icon: Option<&Icon>,
    text: Option<&str>,
) -> Result<Container, Error> {
    let icon = match (icon, template.and_then(|c| c.ext_header(EXT_ICON))) {
        (Some(i), _) if i.size == 40 => i.encode(),
        (Some(_), _) => return Err(Error::Format("a message icon is 40x40".into())),
        (None, Some(raw)) if raw.len() == ICON_LEN => raw.to_vec(),
        _ => Icon::standard(40).encode(),
    };
    let text = text
        .map(str::to_string)
        .or_else(|| template.and_then(message_text))
        .unwrap_or_else(|| title.message_text().into());
    let text = crate::cec::container::info_text(&text);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    crate::cec::container::Draft {
        title: title.id(),
        body,
        ext_headers: &[(EXT_ICON, &icon), (EXT_INFO_TEXT, &text)],
        message_id: random_id(),
        created: Timestamp::from_unix(now.as_secs(), now.subsec_millis() as u16),
        send_mode: title.send_mode(),
        flags: title.message_flags(),
    }
    .build(key)
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEXT: Field = Field {
        key: "t",
        label: "T",
        kind: Kind::Text {
            max: 3,
            forbid: "%",
        },
        help: "",
        advanced: false,
    };
    const NUMBER: Field = Field {
        key: "n",
        label: "N",
        kind: Kind::Number {
            min: 1,
            max: 10,
            exclude: &[5],
            hex: false,
        },
        help: "",
        advanced: false,
    };

    #[test]
    fn checks_values() {
        assert!(check(&TEXT, &Value::from("abc")).is_ok());
        assert!(check(&TEXT, &Value::from("abcd")).is_err());
        assert!(check(&TEXT, &Value::from("a%")).is_err());
        assert!(check(&TEXT, &Value::from(3)).is_err());
        assert_eq!(from_text(&NUMBER, "0x0a").unwrap(), Value::from(10));
        assert!(from_text(&NUMBER, "5").is_err());
        assert!(from_text(&NUMBER, "11").is_err());
        assert!(from_text(&NUMBER, "x").is_err());
    }

    #[test]
    fn finds_titles() {
        assert_eq!(find("00020800").map(|t| t.id()), Some(0x0002_0800));
        assert_eq!(find("mii plaza").map(|t| t.id()), Some(0x0002_0800));
        assert!(find("00030600").is_none());
        for t in all() {
            for f in t.fields() {
                assert_eq!(t.fields().iter().filter(|g| g.key == f.key).count(), 1);
            }
        }
    }
}
