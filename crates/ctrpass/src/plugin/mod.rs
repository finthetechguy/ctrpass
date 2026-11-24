//! Plugins that describe a title's message body: layout files and WebAssembly modules.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use serde::Deserialize;
use serde::de::{self, Deserializer, Visitor};

use crate::Error;
use crate::title::{Field, Kind, Manifest, MiiPart, MiiSlot, Source, Title, Values};

mod layout;
mod wasm;

#[cfg(test)]
mod tests;

/// The plugin interface version this ctrpass reads.
pub const API: u64 = 1;
/// Largest plugin file loaded.
const MAX_FILE: u64 = 16 << 20;

const BUILT_IN: &[(&str, &str)] = &[(
    "mii-plaza.toml",
    include_str!("../../titles/mii-plaza.toml"),
)];

/// The code behind a title's fields.
pub(crate) enum Engine {
    Layout(layout::Layout),
    Module(wasm::Module),
}

impl Engine {
    /// A new body, and values to apply to it, from the identity's plain values.
    pub(crate) fn create(&self, identity: &Values) -> Result<(Vec<u8>, Values), Error> {
        match self {
            Engine::Layout(l) => Ok(l.create(identity)),
            Engine::Module(m) => Ok((m.create(identity)?, Values::new())),
        }
    }

    pub(crate) fn read(&self, body: &[u8]) -> Result<Values, Error> {
        match self {
            Engine::Layout(l) => l.read(body),
            Engine::Module(m) => m.read(body),
        }
    }

    pub(crate) fn write(&self, body: &[u8], values: &Values) -> Result<Vec<u8>, Error> {
        match self {
            Engine::Layout(l) => l.write(body, values),
            Engine::Module(m) => m.write(body, values),
        }
    }

    pub(crate) fn problems(&self, body: &[u8]) -> Result<Vec<String>, Error> {
        match self {
            Engine::Layout(l) => Ok(l.problems(body)),
            Engine::Module(m) => m.problems(body),
        }
    }
}

/// A whole number written as an integer, or as text in decimal or `0x` hex.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Num(pub u64);

impl Num {
    fn usize(self) -> usize {
        self.0 as usize
    }
}

fn parse_num(s: &str) -> Option<u64> {
    let s = s.trim().replace('_', "");
    match s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        Some(h) => u64::from_str_radix(h, 16).ok(),
        None => s.parse().ok(),
    }
}

impl<'de> Deserialize<'de> for Num {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl Visitor<'_> for V {
            type Value = Num;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("a whole number, or text such as \"0x1f\"")
            }
            fn visit_u64<E: de::Error>(self, v: u64) -> Result<Num, E> {
                Ok(Num(v))
            }
            fn visit_i64<E: de::Error>(self, v: i64) -> Result<Num, E> {
                u64::try_from(v)
                    .map(Num)
                    .map_err(|_| E::custom(format!("{v} is below zero")))
            }
            fn visit_str<E: de::Error>(self, v: &str) -> Result<Num, E> {
                parse_num(v)
                    .map(Num)
                    .ok_or_else(|| E::custom(format!("{v:?} is not a number")))
            }
        }
        d.deserialize_any(V)
    }
}

/// A title ID, written in hex as text or as an integer.
#[derive(Clone, Copy, Debug)]
struct TitleId(u32);

impl<'de> Deserialize<'de> for TitleId {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Raw {
            Int(u64),
            Text(String),
        }
        let n = match Raw::deserialize(d)? {
            Raw::Int(n) => n,
            Raw::Text(s) => {
                let s = s.trim();
                u64::from_str_radix(s.strip_prefix("0x").unwrap_or(s), 16)
                    .map_err(|_| de::Error::custom(format!("{s:?} is not a hex title ID")))?
            }
        };
        match u32::try_from(n) {
            Ok(id) if id != 0 => Ok(TitleId(id)),
            _ => Err(de::Error::custom(format!("{n:#x} is not a title ID"))),
        }
    }
}

fn default_flags() -> u8 {
    3
}

/// A plugin file, or the manifest a module returns.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Spec {
    api: u64,
    title: TitleId,
    name: String,
    version: String,
    author: String,
    #[serde(default)]
    send_mode: u8,
    message_text: String,
    #[serde(default = "default_flags")]
    message_flags: u8,
    #[serde(default, rename = "field", alias = "fields")]
    pub(crate) fields: Vec<FieldSpec>,
    pub(crate) size: Option<Num>,
    #[serde(default)]
    pub(crate) fill: Vec<layout::FillSpec>,
    pub(crate) create: Option<layout::CreateSpec>,
    #[serde(default)]
    pub(crate) check: Vec<layout::CheckSpec>,
    #[serde(default)]
    pub(crate) computed: Vec<layout::ComputedSpec>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct FieldSpec {
    pub(crate) key: String,
    label: String,
    #[serde(default)]
    help: String,
    #[serde(default)]
    advanced: bool,
    #[serde(rename = "type")]
    pub(crate) kind: String,
    pub(crate) at: Option<Num>,
    pub(crate) max: Option<Num>,
    pub(crate) bytes: Option<Num>,
    #[serde(default)]
    forbid: String,
    pub(crate) width: Option<Num>,
    #[serde(default)]
    pub(crate) big_endian: bool,
    min: Option<Num>,
    #[serde(default)]
    exclude: Vec<Num>,
    #[serde(default)]
    hex: bool,
    pub(crate) len: Option<Num>,
    pub(crate) size: Option<Num>,
    pub(crate) scale_from: Option<String>,
    mii: Option<String>,
    pub(crate) copies: Option<Num>,
    pub(crate) stride: Option<Num>,
    pub(crate) read_copy: Option<Num>,
    #[serde(default)]
    pub(crate) clear: Vec<[Num; 2]>,
}

/// Largest value a number of `width` bytes holds.
pub(crate) fn width_max(width: usize) -> u64 {
    match width {
        8 => u64::MAX,
        w => (1u64 << (8 * w)) - 1,
    }
}

fn field_kind(f: &FieldSpec) -> Result<Kind, String> {
    let need = |v: Option<Num>, what: &str| v.map(Num::usize).ok_or(format!("needs {what}"));
    Ok(match f.kind.as_str() {
        "text" => {
            let room = f.bytes.map(|b| (b.usize() / 2).saturating_sub(1));
            let max = match (f.max, room) {
                (Some(m), Some(r)) if m.usize() > r + 1 => {
                    return Err(format!("max {} does not fit in {r} characters", m.0));
                }
                (Some(m), _) => m.usize(),
                (None, Some(r)) => r,
                (None, None) => return Err("needs max or bytes".into()),
            };
            Kind::Text {
                max,
                forbid: f.forbid.clone(),
            }
        }
        "number" => {
            let top = match f.width {
                Some(Num(w @ (1 | 2 | 4 | 8))) => width_max(w as usize),
                Some(w) => return Err(format!("width {} is not 1, 2, 4 or 8", w.0)),
                None => u64::MAX,
            };
            let (min, max) = (f.min.map_or(0, |n| n.0), f.max.map_or(top, |n| n.0));
            if min > max || max > top {
                return Err(format!("range {min}..={max} does not fit the width"));
            }
            Kind::Number {
                min,
                max,
                exclude: f.exclude.iter().map(|n| n.0).collect(),
                hex: f.hex,
            }
        }
        "bytes" => Kind::Bytes {
            len: need(f.len, "len")?,
        },
        "image" => {
            let size = need(f.size, "size")?;
            if size == 0 || size % 8 != 0 || size > 256 {
                return Err(format!("size {size} is not a multiple of 8 up to 256"));
            }
            Kind::Image { size }
        }
        "mii" => Kind::Mii {
            len: crate::mii::RECORD_SIZE,
        },
        other => match MiiPart::named(other) {
            Some(part) => part.kind(),
            None => return Err(format!("type {other:?} is unknown")),
        },
    })
}

/// The fields a spec declares, and the Mii slots among them.
fn fields(specs: &[FieldSpec]) -> Result<(Vec<Field>, Vec<MiiSlot>), Error> {
    let mut fields: Vec<Field> = Vec::new();
    let mut miis: Vec<MiiSlot> = Vec::new();
    for f in specs {
        let bad = |why: String| Error::Format(format!("field {:?}: {why}", f.key));
        if f.key.is_empty() || fields.iter().any(|g| g.key == f.key) {
            return Err(bad("the key is empty or used twice".into()));
        }
        let kind = field_kind(f).map_err(bad)?;
        if f.kind == "mii" {
            let at = f.at.ok_or_else(|| bad("needs at".into()))?;
            miis.push(MiiSlot {
                key: f.key.clone(),
                at: at.usize(),
                parts: Vec::new(),
            });
        }
        fields.push(Field {
            key: f.key.clone(),
            label: f.label.clone(),
            kind,
            help: f.help.clone(),
            advanced: f.advanced,
        });
    }
    for f in specs {
        let Some(part) = MiiPart::named(&f.kind) else {
            continue;
        };
        let bad = |why: &str| Error::Format(format!("field {:?}: {why}", f.key));
        let slot = match &f.mii {
            Some(k) => miis.iter_mut().find(|m| &m.key == k),
            None if miis.len() == 1 => miis.first_mut(),
            None => return Err(bad("needs mii, the key of its Mii field")),
        };
        slot.ok_or_else(|| bad("names no Mii field"))?
            .parts
            .push((f.key.clone(), part));
    }
    Ok((fields, miis))
}

fn manifest(spec: &Spec) -> Result<Manifest, Error> {
    if spec.api != API {
        return Err(Error::Format(format!(
            "plugin API {} is not supported; this ctrpass reads API {API}",
            spec.api
        )));
    }
    if spec.name.trim().is_empty() {
        return Err(Error::Format("the name is empty".into()));
    }
    Ok(Manifest {
        id: spec.title.0,
        name: spec.name.clone(),
        version: spec.version.clone(),
        author: spec.author.clone(),
        send_mode: spec.send_mode,
        message_text: spec.message_text.clone(),
        message_flags: spec.message_flags,
    })
}

/// Reads a layout file's text.
pub fn parse_layout(text: &str, source: Source) -> Result<Title, Error> {
    let spec: Spec = toml::from_str(text).map_err(|e| {
        let line = e.span().map(|s| text[..s.start].matches('\n').count() + 1);
        Error::Format(match line {
            Some(n) => format!("line {n}: {}", e.message()),
            None => e.message().into(),
        })
    })?;
    let manifest = manifest(&spec)?;
    let (fields, miis) = fields(&spec.fields)?;
    let layout = layout::Layout::new(&spec, &fields, &miis)?;
    let title = Title {
        manifest,
        source,
        fields,
        miis,
        engine: Engine::Layout(layout),
    };
    layout::check_create(&title)?;
    Ok(title)
}

/// Reads a WebAssembly module.
pub fn parse_module(wasm: &[u8], source: Source) -> Result<Title, Error> {
    let module = wasm::Module::new(wasm)?;
    let spec: Spec = serde_json::from_slice(&module.manifest()?)
        .map_err(|e| Error::Format(format!("manifest: {e}")))?;
    if spec.size.is_some()
        || spec.create.is_some()
        || !(spec.fill.is_empty() && spec.check.is_empty() && spec.computed.is_empty())
    {
        return Err(Error::Format(
            "size, fill, create, check and computed are for layout files only".into(),
        ));
    }
    let manifest = manifest(&spec)?;
    let (fields, miis) = fields(&spec.fields)?;
    Ok(Title {
        manifest,
        source,
        fields,
        miis,
        engine: Engine::Module(module.named(&spec.name)),
    })
}

/// Loads one plugin file, a layout (`.toml`) or a module (`.wasm`).
pub fn load_file(path: &Path) -> Result<Title, Error> {
    let size = std::fs::metadata(path)?.len();
    if size > MAX_FILE {
        return Err(Error::Format(format!(
            "{size} bytes is over the {MAX_FILE} byte limit"
        )));
    }
    let source = Source::File(path.to_path_buf());
    match path.extension().and_then(|e| e.to_str()) {
        Some("toml") => parse_layout(&std::fs::read_to_string(path)?, source),
        Some("wasm") => parse_module(&std::fs::read(path)?, source),
        _ => Err(Error::Format("a plugin is a .toml or .wasm file".into())),
    }
}

/// A plugin that did not load.
#[derive(Clone, Debug)]
pub struct LoadError {
    pub file: PathBuf,
    pub error: String,
}

#[derive(Debug, Default)]
pub struct Registry {
    titles: Vec<Title>,
    errors: Vec<LoadError>,
    warnings: Vec<String>,
}

impl Registry {
    /// The built-in plugins, then each plugin in `dir`, when given.
    pub fn load(dir: Option<&Path>) -> Self {
        let mut r = Registry::default();
        for (name, text) in BUILT_IN {
            match parse_layout(text, Source::BuiltIn) {
                Ok(t) => r.titles.push(t),
                Err(e) => r.errors.push(LoadError {
                    file: name.into(),
                    error: e.to_string(),
                }),
            }
        }
        if let Some(dir) = dir {
            r.load_dir(dir);
        }
        r
    }

    fn load_dir(&mut self, dir: &Path) {
        let entries = match std::fs::read_dir(dir) {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return,
            Err(e) => {
                self.errors.push(LoadError {
                    file: dir.into(),
                    error: e.to_string(),
                });
                return;
            }
        };
        let mut paths: Vec<PathBuf> = entries
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| {
                matches!(
                    p.extension().and_then(|e| e.to_str()),
                    Some("toml" | "wasm")
                )
            })
            .collect();
        paths.sort();
        for path in paths {
            match load_file(&path) {
                Ok(t) => self.add(t),
                Err(e) => self.errors.push(LoadError {
                    file: path,
                    error: e.to_string(),
                }),
            }
        }
    }

    /// Adds a loaded plugin; it replaces a built-in one for the same title, but not another file.
    fn add(&mut self, t: Title) {
        let path = t.source().path().unwrap_or(Path::new("")).to_path_buf();
        match self.titles.iter().position(|o| o.id() == t.id()) {
            Some(i) if self.titles[i].source() == &Source::BuiltIn => {
                self.warnings.push(format!(
                    "{} replaces the built-in {} plugin",
                    path.display(),
                    self.titles[i].name()
                ));
                self.titles[i] = t;
            }
            Some(i) => self.errors.push(LoadError {
                file: path,
                error: format!(
                    "title {:08X} already has a plugin, {}",
                    t.id(),
                    self.titles[i]
                        .source()
                        .path()
                        .map_or_else(String::new, |p| p.display().to_string())
                ),
            }),
            None => self.titles.push(t),
        }
    }

    pub fn all(&self) -> &[Title] {
        &self.titles
    }

    pub fn errors(&self) -> &[LoadError] {
        &self.errors
    }

    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }

    /// Finds a title by hex ID or by name, ignoring case.
    pub fn find(&self, id_or_name: &str) -> Option<&Title> {
        let id = u32::from_str_radix(id_or_name.trim_start_matches("0x"), 16).ok();
        self.titles
            .iter()
            .find(|t| Some(t.id()) == id || t.name().eq_ignore_ascii_case(id_or_name))
    }

    pub fn by_id(&self, id: u32) -> Option<&Title> {
        self.titles.iter().find(|t| t.id() == id)
    }
}

/// The plugin folder: `$CTRPASS_PLUGINS`, else `ctrpass/titles` in the per-user data folder.
pub fn default_dir() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("CTRPASS_PLUGINS") {
        return Some(p.into());
    }
    std::env::var_os("XDG_DATA_HOME")
        .filter(|d| !d.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| Path::new(&h).join(".local/share")))
        .map(|d| d.join("ctrpass/titles"))
}

static REGISTRY: OnceLock<Registry> = OnceLock::new();

/// The titles from the built-in plugins and the plugin folder, loaded on first use.
pub fn registry() -> &'static Registry {
    REGISTRY.get_or_init(|| Registry::load(default_dir().as_deref()))
}
