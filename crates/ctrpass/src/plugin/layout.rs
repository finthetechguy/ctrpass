//! Layout files: each field at a fixed spot in a body of fixed size.

use std::collections::BTreeMap;

use serde::Deserialize;
use serde_json::Value;

use super::{FieldSpec, Num, Spec, parse_num, width_max};
use crate::Error;
use crate::cec::container::MAX_BODY;
use crate::cec::icon::Icon;
use crate::mii::WRAPPED_SIZE;
use crate::title::{self, Field, Kind, MiiPart, MiiSlot, Title, Values, hex, identity_values};

/// Bytes a new body starts with.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct FillSpec {
    at: Num,
    hex: String,
}

/// Values a new body gets, given directly or taken from the identity.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CreateSpec {
    #[serde(default)]
    values: Values,
    #[serde(default)]
    identity: BTreeMap<String, String>,
}

/// Bytes the body must hold for the game to keep it.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CheckSpec {
    at: Num,
    one_of: Vec<String>,
    problem: String,
}

/// A number ctrpass works out from the body after each change.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ComputedSpec {
    #[serde(rename = "type")]
    kind: String,
    at: Num,
    width: Option<Num>,
    #[serde(default)]
    big_endian: bool,
    start: Option<Num>,
    end: Option<Num>,
    stride: Option<Num>,
    entries: Option<Num>,
    field: Option<String>,
}

#[derive(Clone, Debug)]
enum How {
    Text {
        bytes: usize,
    },
    Number {
        width: usize,
        big: bool,
    },
    Bytes {
        len: usize,
    },
    Image {
        size: usize,
        scale_from: Option<String>,
    },
}

#[derive(Clone, Debug)]
struct Place {
    key: String,
    at: usize,
    copies: usize,
    stride: usize,
    read_copy: usize,
    clear: Vec<(usize, usize)>,
    how: How,
}

impl Place {
    fn extent(&self) -> usize {
        match self.how {
            How::Text { bytes } => bytes,
            How::Number { width, .. } => width,
            How::Bytes { len } => len,
            How::Image { size, .. } => Icon::byte_len(size),
        }
    }

    fn span(&self, copy: usize) -> std::ops::Range<usize> {
        let at = self.at + copy * self.stride;
        at..at + self.extent()
    }

    fn read(&self, body: &[u8]) -> Value {
        let b = &body[self.span(self.read_copy)];
        match self.how {
            How::Text { .. } => {
                let units: Vec<u16> = b
                    .chunks_exact(2)
                    .map(|c| u16::from_le_bytes([c[0], c[1]]))
                    .take_while(|&u| u != 0)
                    .collect();
                String::from_utf16_lossy(&units).into()
            }
            How::Number { big, .. } => get_uint(b, big).into(),
            How::Bytes { .. } | How::Image { .. } => hex(b).into(),
        }
    }

    fn encode(&self, v: &Value) -> Result<Vec<u8>, Error> {
        let bad = || Error::Format(format!("{}: {v} does not fit", self.key));
        Ok(match self.how {
            How::Text { bytes } => {
                let mut out: Vec<u8> = v
                    .as_str()
                    .ok_or_else(bad)?
                    .encode_utf16()
                    .flat_map(u16::to_le_bytes)
                    .collect();
                if out.len() > bytes {
                    return Err(bad());
                }
                out.resize(bytes, 0);
                out
            }
            How::Number { width, big } => {
                let n = v
                    .as_u64()
                    .filter(|&n| n <= width_max(width))
                    .ok_or_else(bad)?;
                put_uint(n, width, big)
            }
            How::Bytes { len } => v
                .as_str()
                .and_then(title::unhex)
                .filter(|b| b.len() == len)
                .ok_or_else(bad)?,
            How::Image { size, .. } => v
                .as_str()
                .and_then(title::unhex)
                .filter(|b| b.len() == Icon::byte_len(size))
                .ok_or_else(bad)?,
        })
    }

    fn write(&self, body: &mut [u8], bytes: &[u8]) {
        for copy in 0..self.copies {
            body[self.span(copy)].copy_from_slice(bytes);
        }
        for &(at, len) in &self.clear {
            body[at..at + len].fill(0);
        }
    }
}

fn get_uint(b: &[u8], big: bool) -> u64 {
    let fold = |n: u64, &x: &u8| n << 8 | u64::from(x);
    if big {
        b.iter().fold(0, fold)
    } else {
        b.iter().rev().fold(0, fold)
    }
}

fn put_uint(n: u64, width: usize, big: bool) -> Vec<u8> {
    let mut out = n.to_le_bytes()[..width].to_vec();
    if big {
        out.reverse();
    }
    out
}

fn crc32(data: &[u8]) -> u32 {
    let mut c = !0u32;
    for &b in data {
        c ^= u32::from(b);
        for _ in 0..8 {
            c = if c & 1 != 0 {
                (c >> 1) ^ 0xedb8_8320
            } else {
                c >> 1
            };
        }
    }
    !c
}

#[derive(Clone, Debug)]
enum Rule {
    Crc32 {
        start: usize,
        end: usize,
    },
    Length {
        place: usize,
    },
    Count {
        start: usize,
        stride: usize,
        entries: usize,
    },
}

#[derive(Clone, Debug)]
struct Computed {
    at: usize,
    width: usize,
    big: bool,
    rule: Rule,
}

impl Computed {
    fn what(&self) -> &'static str {
        match self.rule {
            Rule::Crc32 { .. } => "checksum",
            Rule::Length { .. } => "length",
            Rule::Count { .. } => "count",
        }
    }

    fn value(&self, body: &[u8], places: &[Place]) -> u64 {
        let n = match self.rule {
            Rule::Crc32 { start, end } => crc32(&body[start..end]).into(),
            Rule::Length { place } => match places[place].read(body) {
                Value::String(s) => s.encode_utf16().count() as u64,
                _ => 0,
            },
            Rule::Count {
                start,
                stride,
                entries,
            } => (0..entries)
                .filter(|i| {
                    let at = start + i * stride;
                    body[at..at + stride].iter().any(|&b| b != 0)
                })
                .count() as u64,
        };
        n.min(width_max(self.width))
    }
}

#[derive(Debug)]
struct Check {
    at: usize,
    one_of: Vec<Vec<u8>>,
    problem: String,
}

#[derive(Debug)]
pub(crate) struct Layout {
    name: String,
    size: usize,
    fill: Vec<(usize, Vec<u8>)>,
    create: Values,
    identity: Vec<(String, String)>,
    places: Vec<Place>,
    checks: Vec<Check>,
    computed: Vec<Computed>,
}

fn unhex_spaced(s: &str) -> Option<Vec<u8>> {
    title::unhex(&s.split_whitespace().collect::<String>())
}

fn place(f: &FieldSpec, kind: &Kind, size: usize) -> Result<Place, String> {
    let at = f.at.ok_or("needs at")?.usize();
    let how = match kind {
        Kind::Text { .. } => {
            let bytes = f.bytes.ok_or("needs bytes")?.usize();
            if bytes < 2 || bytes % 2 != 0 {
                return Err(format!("bytes {bytes} is not an even number above 0"));
            }
            How::Text { bytes }
        }
        Kind::Number { .. } => How::Number {
            width: f.width.ok_or("needs width")?.usize(),
            big: f.big_endian,
        },
        Kind::Bytes { len } => How::Bytes { len: *len },
        Kind::Image { size } => How::Image {
            size: *size,
            scale_from: f.scale_from.clone(),
        },
        Kind::Mii { .. } => unreachable!("Mii fields have no place"),
    };
    let mut p = Place {
        key: f.key.clone(),
        at,
        copies: f.copies.map_or(1, Num::usize),
        stride: 0,
        read_copy: f.read_copy.map_or(0, Num::usize),
        clear: f
            .clear
            .iter()
            .map(|[a, l]| (a.usize(), l.usize()))
            .collect(),
        how,
    };
    p.stride = f.stride.map_or(p.extent(), Num::usize);
    if p.copies == 0 || p.read_copy >= p.copies {
        return Err("copies must be above 0 and read_copy below copies".into());
    }
    if p.copies > 1 && p.stride < p.extent() {
        return Err(format!("stride {:#x} is shorter than one copy", p.stride));
    }
    if p.span(p.copies - 1).end > size {
        return Err("lies past the end of the body".into());
    }
    if p.clear.iter().any(|&(a, l)| a + l > size) {
        return Err("clears bytes past the end of the body".into());
    }
    Ok(p)
}

fn computed(c: &ComputedSpec, size: usize, places: &[Place]) -> Result<Computed, String> {
    let need = |v: Option<Num>, what: &str| v.map(Num::usize).ok_or(format!("needs {what}"));
    let width = match c.kind.as_str() {
        "crc32" => 4,
        _ => match need(c.width, "width")? {
            w @ (1 | 2 | 4 | 8) => w,
            w => return Err(format!("width {w} is not 1, 2, 4 or 8")),
        },
    };
    let at = c.at.usize();
    let rule = match c.kind.as_str() {
        "crc32" => {
            let (start, end) = (
                c.start.map_or(0, Num::usize),
                c.end.map_or(size, Num::usize),
            );
            if start >= end || end > size {
                return Err(format!("range {start:#x}..{end:#x} is not in the body"));
            }
            if at < end && start < at + width {
                return Err("the checksum lies inside its own range".into());
            }
            Rule::Crc32 { start, end }
        }
        "length" => {
            let key = c.field.as_deref().ok_or("needs field")?;
            let place = places
                .iter()
                .position(|p| p.key == key && matches!(p.how, How::Text { .. }))
                .ok_or(format!("{key:?} is not a text field"))?;
            Rule::Length { place }
        }
        "count" => {
            let (start, stride, entries) = (
                need(c.start, "start")?,
                need(c.stride, "stride")?,
                need(c.entries, "entries")?,
            );
            if stride == 0 || start + stride * entries > size {
                return Err("the entries are not in the body".into());
            }
            Rule::Count {
                start,
                stride,
                entries,
            }
        }
        other => return Err(format!("type {other:?} is unknown")),
    };
    if at + width > size {
        return Err("lies past the end of the body".into());
    }
    Ok(Computed {
        at,
        width,
        big: c.big_endian,
        rule,
    })
}

impl Layout {
    pub(crate) fn new(spec: &Spec, fields: &[Field], miis: &[MiiSlot]) -> Result<Self, Error> {
        let fail = |what: String| Error::Format(what);
        let size = spec
            .size
            .ok_or_else(|| fail("a layout file needs size".into()))?
            .usize();
        if size == 0 || size > MAX_BODY {
            return Err(fail(format!("size {size:#x} is outside 1..={MAX_BODY:#x}")));
        }
        let mut fill = Vec::new();
        for f in &spec.fill {
            let bytes = unhex_spaced(&f.hex)
                .ok_or_else(|| fail(format!("fill at {:#x} is not hex", f.at.0)))?;
            if f.at.usize() + bytes.len() > size {
                return Err(fail(format!("fill at {:#x} runs past the body", f.at.0)));
            }
            fill.push((f.at.usize(), bytes));
        }

        let mut places = Vec::new();
        for (f, field) in spec.fields.iter().zip(fields) {
            if matches!(field.kind, Kind::Mii { .. }) || MiiPart::named(&f.kind).is_some() {
                continue;
            }
            let p = place(f, &field.kind, size)
                .map_err(|why| fail(format!("field {:?}: {why}", f.key)))?;
            places.push(p);
        }
        for p in &places {
            let How::Image {
                scale_from: Some(src),
                ..
            } = &p.how
            else {
                continue;
            };
            if !places
                .iter()
                .any(|q| &q.key == src && matches!(q.how, How::Image { .. }))
            {
                return Err(fail(format!(
                    "field {:?}: scale_from {src:?} is not an image field",
                    p.key
                )));
            }
        }
        for m in miis {
            if m.at + WRAPPED_SIZE > size {
                return Err(fail(format!(
                    "field {:?}: lies past the end of the body",
                    m.key
                )));
            }
        }

        let mut checks = Vec::new();
        for c in &spec.check {
            let one_of: Option<Vec<Vec<u8>>> = c.one_of.iter().map(|h| unhex_spaced(h)).collect();
            let one_of = one_of
                .filter(|o| !o.is_empty())
                .ok_or_else(|| fail(format!("check at {:#x}: one_of needs hex values", c.at.0)))?;
            if one_of.iter().any(|b| c.at.usize() + b.len() > size) {
                return Err(fail(format!("check at {:#x} runs past the body", c.at.0)));
            }
            checks.push(Check {
                at: c.at.usize(),
                one_of,
                problem: c.problem.clone(),
            });
        }

        let mut comp = Vec::new();
        for c in &spec.computed {
            comp.push(
                computed(c, size, &places)
                    .map_err(|why| fail(format!("computed at {:#x}: {why}", c.at.0)))?,
            );
        }
        // Checksums go last so they cover the other computed values.
        comp.sort_by_key(|c| matches!(c.rule, Rule::Crc32 { .. }));

        let mut create = Values::new();
        let mut identity = Vec::new();
        if let Some(c) = &spec.create {
            for (k, v) in &c.values {
                let field = fields
                    .iter()
                    .find(|f| &f.key == k)
                    .ok_or_else(|| fail(format!("create: no field {k:?}")))?;
                let v = match (&field.kind, v.as_str()) {
                    (Kind::Image { size }, Some("standard")) => {
                        hex(&Icon::standard(*size).encode()).into()
                    }
                    (Kind::Number { .. }, Some(s)) => parse_num(s)
                        .ok_or_else(|| fail(format!("create: {k} {s:?} is not a number")))?
                        .into(),
                    _ => v.clone(),
                };
                create.insert(k.clone(), v);
            }
            let known = identity_values(&crate::identity::Identity::from_seed(&[0; 32]));
            for (k, name) in &c.identity {
                let field = fields
                    .iter()
                    .find(|f| &f.key == k)
                    .ok_or_else(|| fail(format!("create: no field {k:?}")))?;
                let sample = known.get(name).ok_or_else(|| {
                    fail(format!(
                        "create: the identity has no {name:?}; it has {}",
                        known.keys().cloned().collect::<Vec<_>>().join(", ")
                    ))
                })?;
                if !matches!(
                    (&field.kind, sample),
                    (Kind::Number { .. }, Value::Number(_))
                        | (Kind::Bytes { len: 6 }, Value::String(_))
                ) {
                    return Err(fail(format!(
                        "create: {k} can't hold the identity's {name}"
                    )));
                }
                identity.push((k.clone(), name.clone()));
            }
        }

        Ok(Layout {
            name: spec.name.clone(),
            size,
            fill,
            create,
            identity,
            places,
            checks,
            computed: comp,
        })
    }

    fn check_size(&self, body: &[u8]) -> Result<(), Error> {
        if body.len() != self.size {
            return Err(Error::Format(format!(
                "a {} body is {:#x} bytes, not {:#x}",
                self.name,
                self.size,
                body.len()
            )));
        }
        Ok(())
    }

    fn compute(&self, body: &mut [u8]) {
        for c in &self.computed {
            let n = c.value(body, &self.places);
            body[c.at..c.at + c.width].copy_from_slice(&put_uint(n, c.width, c.big));
        }
    }

    pub(crate) fn create(&self, identity: &Values) -> (Vec<u8>, Values) {
        let mut body = vec![0; self.size];
        for (at, bytes) in &self.fill {
            body[*at..at + bytes.len()].copy_from_slice(bytes);
        }
        self.compute(&mut body);
        let mut values = self.create.clone();
        for (k, name) in &self.identity {
            if let Some(v) = identity.get(name) {
                values.insert(k.clone(), v.clone());
            }
        }
        (body, values)
    }

    pub(crate) fn read(&self, body: &[u8]) -> Result<Values, Error> {
        self.check_size(body)?;
        Ok(self
            .places
            .iter()
            .map(|p| (p.key.clone(), p.read(body)))
            .collect())
    }

    pub(crate) fn write(&self, body: &[u8], values: &Values) -> Result<Vec<u8>, Error> {
        self.check_size(body)?;
        let mut out = body.to_vec();
        for (k, v) in values {
            let p = self
                .places
                .iter()
                .find(|p| &p.key == k)
                .ok_or_else(|| Error::Format(format!("{} has no field {k:?}", self.name)))?;
            p.write(&mut out, &p.encode(v)?);
        }
        for p in &self.places {
            let How::Image {
                size,
                scale_from: Some(src),
            } = &p.how
            else {
                continue;
            };
            if values.contains_key(&p.key) {
                continue;
            }
            let Some(from) = values
                .get(src)
                .and_then(Value::as_str)
                .and_then(title::unhex)
            else {
                continue;
            };
            let from_size = match self.places.iter().find(|q| &q.key == src).map(|q| &q.how) {
                Some(How::Image { size, .. }) => *size,
                _ => continue,
            };
            let icon = Icon::decode(from_size, &from)?.resized(*size);
            p.write(&mut out, &icon.encode());
        }
        self.compute(&mut out);
        Ok(out)
    }

    pub(crate) fn problems(&self, body: &[u8]) -> Vec<String> {
        if let Err(e) = self.check_size(body) {
            return vec![e.to_string()];
        }
        let mut out = Vec::new();
        for c in &self.checks {
            if !c
                .one_of
                .iter()
                .any(|want| body[c.at..c.at + want.len()] == want[..])
            {
                out.push(c.problem.clone());
            }
        }
        for c in &self.computed {
            let held = get_uint(&body[c.at..c.at + c.width], c.big);
            if held != c.value(body, &self.places) {
                out.push(format!("the {} at {:#x} does not match", c.what(), c.at));
            }
        }
        out
    }
}

/// Checks the values a new body gets against their fields.
pub(crate) fn check_create(title: &Title) -> Result<(), Error> {
    if let super::Engine::Layout(l) = &title.engine {
        title::check_all(title, &l.create).map_err(|e| Error::Format(format!("create: {e}")))?;
    }
    Ok(())
}
