//! Titles whose message bodies ctrpass can create and edit, each described by a plugin.

use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::{Map, Value};

use crate::Error;
use crate::cec::Container;
use crate::cec::container::{EXT_ICON, EXT_INFO_TEXT, ICON_LEN, MAX_BODY, Timestamp};
use crate::cec::icon::Icon;
use crate::identity::Identity;
use crate::keys::{Key, TitleKey};
use crate::mii::{self, NAME_LEN, Record, STORED_SIZE, WRAPPED_SIZE};
use crate::plugin::Engine;

/// Field values by key. Images are hex of their tiled bytes, byte strings and Miis are hex.
pub type Values = Map<String, Value>;

/// Mii names may not hold these characters.
const MII_NAME_FORBID: &str = "%\\";
/// Name a new Mii is given before any values are applied.
const NEW_MII_NAME: &str = "ctrpass";

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Kind {
    /// Text of at most `max` UTF-16 units, without the characters in `forbid`.
    Text { max: usize, forbid: String },
    /// A whole number shown in decimal, or hex when `hex` is set.
    Number {
        min: u64,
        max: u64,
        exclude: Vec<u64>,
        hex: bool,
    },
    /// A square image of `size` pixels.
    Image { size: usize },
    /// Exactly `len` bytes.
    Bytes { len: usize },
    /// A Mii record of `len` bytes, kept protected in the body by ctrpass.
    Mii { len: usize },
}

#[derive(Clone, Debug, Serialize)]
pub struct Field {
    pub key: String,
    pub label: String,
    pub kind: Kind,
    pub help: String,
    /// Rarely changed, so front ends may tuck it away.
    pub advanced: bool,
}

/// A value ctrpass reads from or writes into a Mii record.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MiiPart {
    Name,
    Creator,
    Id,
    Mac,
}

impl MiiPart {
    pub fn named(name: &str) -> Option<Self> {
        Some(match name {
            "mii_name" => MiiPart::Name,
            "mii_creator" => MiiPart::Creator,
            "mii_id" => MiiPart::Id,
            "mii_mac" => MiiPart::Mac,
            _ => return None,
        })
    }

    pub fn kind(self) -> Kind {
        match self {
            MiiPart::Name | MiiPart::Creator => Kind::Text {
                max: NAME_LEN,
                forbid: MII_NAME_FORBID.into(),
            },
            MiiPart::Id => Kind::Number {
                min: 0,
                max: u32::MAX.into(),
                exclude: Vec::new(),
                hex: true,
            },
            MiiPart::Mac => Kind::Bytes { len: 6 },
        }
    }
}

/// A protected Mii record at `at` in the body, and the fields that read parts of it.
#[derive(Clone, Debug)]
pub struct MiiSlot {
    pub key: String,
    pub at: usize,
    pub parts: Vec<(String, MiiPart)>,
}

impl MiiSlot {
    fn holds(&self, key: &str) -> bool {
        self.key == key || self.parts.iter().any(|(k, _)| k == key)
    }

    fn range(&self, body: &[u8]) -> Result<std::ops::Range<usize>, Error> {
        let r = self.at..self.at + WRAPPED_SIZE;
        if r.end > body.len() {
            return Err(Error::Format(format!(
                "{} lies past the end of the body",
                self.key
            )));
        }
        Ok(r)
    }

    fn read(&self, body: &[u8], key: &Key, out: &mut Values) -> Result<(), Error> {
        let mii = mii::unwrap(key, &body[self.range(body)?])?;
        out.insert(self.key.clone(), hex(mii.as_bytes()).into());
        for (k, part) in &self.parts {
            let v = match part {
                MiiPart::Name => mii.name().into(),
                MiiPart::Creator => mii.creator_name().into(),
                MiiPart::Id => mii.mii_id().into(),
                MiiPart::Mac => hex(&mii.creator_mac()).into(),
            };
            out.insert(k.clone(), v);
        }
        Ok(())
    }

    /// Puts `mii` into the body, refusing one a receiver would drop.
    fn place(&self, body: &mut [u8], key: &Key, mii: &Record) -> Result<(), Error> {
        let range = self.range(body)?;
        let mii = mii.for_sending();
        if let Some(issue) = mii.validate().first() {
            return Err(Error::Format(format!("{}: {issue}", self.key)));
        }
        body[range].copy_from_slice(&mii::wrap(key, &mii));
        Ok(())
    }

    fn write(&self, body: &mut [u8], key: &Key, values: &Values) -> Result<(), Error> {
        let g = Get(values);
        let mut mii = match g.bytes(&self.key) {
            Some(b) => Record::from_bytes(&b)?,
            None => mii::unwrap(key, &body[self.range(body)?])?,
        };
        let (mut id, mut mac) = (mii.mii_id(), mii.creator_mac());
        for (k, part) in &self.parts {
            match part {
                MiiPart::Name => g.text(k).map(|n| mii.set_name(n)).transpose()?,
                MiiPart::Creator => g.text(k).map(|n| mii.set_creator_name(n)).transpose()?,
                MiiPart::Id => g.number(k).map(|n| id = n as u32),
                MiiPart::Mac => g.bytes(k).map(|b| mac = b.try_into().unwrap()),
            };
        }
        mii.set_identity(id, mac);
        self.place(body, key, &mii)
    }
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

    fn identity(&self) -> Result<&Identity, Error> {
        self.identity
            .ok_or_else(|| Error::Format("a new message needs an identity".into()))
    }
}

/// The identity as plain values, the only part of it a plugin sees.
pub fn identity_values(id: &Identity) -> Values {
    let mut v = Values::new();
    v.insert("mii_id".into(), id.mii_id.into());
    v.insert("creator_mac".into(), hex(&id.creator_mac).into());
    v.insert("face_shape".into(), id.face_shape.into());
    v.insert("gender".into(), id.gender.into());
    v.insert("hat".into(), id.hat.into());
    v.insert("dream".into(), id.dream.into());
    v.insert("hobby".into(), id.hobby.into());
    v
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Source {
    BuiltIn,
    File(PathBuf),
}

impl Source {
    pub fn path(&self) -> Option<&Path> {
        match self {
            Source::BuiltIn => None,
            Source::File(p) => Some(p),
        }
    }
}

/// A plugin's description of itself.
#[derive(Clone, Debug)]
pub struct Manifest {
    pub id: u32,
    pub name: String,
    pub version: String,
    pub author: String,
    pub send_mode: u8,
    pub message_text: String,
    pub message_flags: u8,
}

pub struct Title {
    pub(crate) manifest: Manifest,
    pub(crate) source: Source,
    pub(crate) fields: Vec<Field>,
    pub(crate) miis: Vec<MiiSlot>,
    pub(crate) engine: Engine,
}

impl std::fmt::Debug for Title {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Title({:08X} {})", self.id(), self.name())
    }
}

impl Title {
    pub fn id(&self) -> u32 {
        self.manifest.id
    }

    pub fn name(&self) -> &str {
        &self.manifest.name
    }

    pub fn version(&self) -> &str {
        &self.manifest.version
    }

    pub fn author(&self) -> &str {
        &self.manifest.author
    }

    pub fn source(&self) -> &Source {
        &self.source
    }

    pub fn send_mode(&self) -> u8 {
        self.manifest.send_mode
    }

    /// Text shown with a new message.
    pub fn message_text(&self) -> &str {
        &self.manifest.message_text
    }

    /// Flags a new message carries.
    pub fn message_flags(&self) -> u8 {
        self.manifest.message_flags
    }

    pub fn fields(&self) -> &[Field] {
        &self.fields
    }

    fn is_mii(&self, key: &str) -> bool {
        self.miis.iter().any(|m| m.holds(key))
    }

    fn check_body(&self, body: &[u8]) -> Result<(), Error> {
        if body.is_empty() || body.len() > MAX_BODY {
            return Err(Error::Format(format!(
                "{} made a body of {:#x} bytes, outside 1..={MAX_BODY:#x}",
                self.name(),
                body.len()
            )));
        }
        Ok(())
    }

    /// A new body, made from the context's identity.
    pub fn create(&self, ctx: &Context) -> Result<Vec<u8>, Error> {
        let id = ctx.identity()?;
        let (mut body, values) = self.engine.create(&identity_values(id))?;
        self.check_body(&body)?;
        if !self.miis.is_empty() {
            let key = ctx.record_key()?;
            let mii = Record::new(
                NEW_MII_NAME,
                id.mii_id,
                id.creator_mac,
                id.face_shape,
                id.gender,
            )?;
            for slot in &self.miis {
                slot.place(&mut body, key, &mii)?;
            }
        }
        if values.is_empty() {
            return Ok(body);
        }
        self.write(&body, &values, ctx)
    }

    /// Every field's value in `body`.
    pub fn read(&self, body: &[u8], ctx: &Context) -> Result<Values, Error> {
        let mut values = self.read_plain(body)?;
        if !self.miis.is_empty() {
            let key = ctx.record_key()?;
            for slot in &self.miis {
                slot.read(body, key, &mut values)?;
            }
        }
        Ok(values)
    }

    /// The values the plugin reads, after checking each is a known field of the right type.
    fn read_plain(&self, body: &[u8]) -> Result<Values, Error> {
        let values = self.engine.read(body)?;
        for (k, v) in &values {
            let f = field(self, k)?;
            if self.is_mii(k) || !same_type(&f.kind, v) {
                return Err(Error::Format(format!(
                    "{} gave {k} the value {v}, which does not fit the field",
                    self.name()
                )));
            }
        }
        Ok(values)
    }

    /// `body` with the given values applied; fields not in `values` keep their bytes.
    pub fn write(&self, body: &[u8], values: &Values, ctx: &Context) -> Result<Vec<u8>, Error> {
        let values = check_all(self, values)?;
        let (miis, plain): (Values, Values) = values.into_iter().partition(|(k, _)| self.is_mii(k));
        let mut body = if plain.is_empty() {
            body.to_vec()
        } else {
            self.engine.write(body, &plain)?
        };
        self.check_body(&body)?;
        for slot in &self.miis {
            let mine: Values = miis
                .iter()
                .filter(|(k, _)| slot.holds(k))
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();
            if !mine.is_empty() {
                slot.write(&mut body, ctx.record_key()?, &mine)?;
            }
        }
        Ok(body)
    }

    /// Values in `body` the game would refuse or reset.
    pub fn problems(&self, body: &[u8]) -> Vec<String> {
        let mut out = match self.read_plain(body) {
            Ok(values) => values
                .iter()
                .filter_map(|(k, v)| check(field(self, k).ok()?, v).err())
                .map(|e| e.to_string())
                .collect(),
            Err(e) => vec![e.to_string()],
        };
        match self.engine.problems(body) {
            Ok(p) => out.extend(p),
            Err(e) => out.push(e.to_string()),
        }
        out
    }
}

/// The registry's titles: built-in plugins, then those in the plugin folder.
pub fn all() -> &'static [Title] {
    crate::plugin::registry().all()
}

/// Finds a title by hex ID or by name, ignoring case.
pub fn find(id_or_name: &str) -> Option<&'static Title> {
    crate::plugin::registry().find(id_or_name)
}

pub fn by_id(id: u32) -> Option<&'static Title> {
    crate::plugin::registry().by_id(id)
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

/// Whether `v` has the JSON type `kind` takes, whatever its value.
fn same_type(kind: &Kind, v: &Value) -> bool {
    match kind {
        Kind::Number { .. } => v.is_u64(),
        _ => v.is_string(),
    }
}

/// Checks a value against its field, returning it in canonical form.
pub fn check(field: &Field, v: &Value) -> Result<Value, Error> {
    match &field.kind {
        Kind::Text { max, forbid } => {
            let s = v
                .as_str()
                .ok_or_else(|| invalid(field, format!("expected text, got {v}")))?;
            let units = s.encode_utf16().count();
            if units > *max {
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
            if n < *min || n > *max {
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
                .is_some_and(|b| b.len() == Icon::byte_len(*size));
            if !ok {
                return Err(invalid(field, format!("expected a {size}x{size} image")));
            }
            Ok(v.clone())
        }
        Kind::Bytes { len } => {
            let ok = v.as_str().and_then(unhex).is_some_and(|b| b.len() == *len);
            if !ok {
                return Err(invalid(field, format!("expected {len} bytes as hex")));
            }
            Ok(v.clone())
        }
        Kind::Mii { .. } => {
            let bytes = v
                .as_str()
                .and_then(unhex)
                .ok_or_else(|| invalid(field, "expected a Mii record as hex".into()))?;
            let mii = match bytes.len() {
                STORED_SIZE => Record::from_stored(&bytes),
                _ => Record::from_bytes(&bytes),
            }
            .map_err(|e| invalid(field, e.to_string()))?;
            Ok(Value::from(hex(mii.as_bytes())))
        }
    }
}

/// Reads a value for `field` from command-line text.
pub fn from_text(field: &Field, text: &str) -> Result<Value, Error> {
    let v = match field.kind {
        Kind::Number { hex, .. } => {
            let t = text.trim();
            let parsed = match t.strip_prefix("0x") {
                Some(h) => u64::from_str_radix(h, 16),
                None if hex => u64::from_str_radix(t, 16),
                None => t.parse(),
            };
            Value::from(parsed.map_err(|_| invalid(field, format!("{text:?} is not a number")))?)
        }
        _ => Value::from(text),
    };
    check(field, &v)
}

/// Reads a Mii or bytes field from a file, unprotecting a Mii when `record_key` is given.
pub fn from_file(field: &Field, bytes: &[u8], record_key: Option<&Key>) -> Result<Value, Error> {
    match field.kind {
        Kind::Mii { .. } if bytes.len() == WRAPPED_SIZE => {
            let key = record_key
                .ok_or_else(|| Error::Keys("a protected Mii needs the slot0x31 key".into()))?;
            check(
                field,
                &Value::from(hex(mii::unwrap(key, bytes)?.as_bytes())),
            )
        }
        Kind::Mii { .. } | Kind::Bytes { .. } => check(field, &Value::from(hex(bytes))),
        _ => Err(invalid(field, "can't be read from a file".into())),
    }
}

/// Looks up `key` in a title's fields.
pub fn field<'a>(title: &'a Title, key: &str) -> Result<&'a Field, Error> {
    title
        .fields()
        .iter()
        .find(|f| f.key == key)
        .ok_or_else(|| Error::Format(format!("{} has no field {key:?}", title.name())))
}

/// Checks every value in `values` against `title`'s fields.
pub fn check_all(title: &Title, values: &Values) -> Result<Values, Error> {
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
    title: &Title,
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

    fn text() -> Field {
        Field {
            key: "t".into(),
            label: "T".into(),
            kind: Kind::Text {
                max: 3,
                forbid: "%".into(),
            },
            help: String::new(),
            advanced: false,
        }
    }

    fn number() -> Field {
        Field {
            key: "n".into(),
            label: "N".into(),
            kind: Kind::Number {
                min: 1,
                max: 10,
                exclude: vec![5],
                hex: false,
            },
            help: String::new(),
            advanced: false,
        }
    }

    #[test]
    fn checks_values() {
        let (t, n) = (text(), number());
        assert!(check(&t, &Value::from("abc")).is_ok());
        assert!(check(&t, &Value::from("abcd")).is_err());
        assert!(check(&t, &Value::from("a%")).is_err());
        assert!(check(&t, &Value::from(3)).is_err());
        assert_eq!(from_text(&n, "0x0a").unwrap(), Value::from(10));
        assert!(from_text(&n, "5").is_err());
        assert!(from_text(&n, "11").is_err());
        assert!(from_text(&n, "x").is_err());
    }

    #[test]
    fn finds_titles() {
        assert_eq!(find("00020800").map(|t| t.id()), Some(0x0002_0800));
        assert_eq!(find("mii plaza").map(|t| t.id()), Some(0x0002_0800));
        assert!(find("00030600").is_none());
    }
}
