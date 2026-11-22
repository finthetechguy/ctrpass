//! Python bindings for the store, messages and title fields.

use std::path::PathBuf;
use std::sync::Mutex;

use ::ctrpass::cec::container::{Container as RsContainer, Timestamp};
use ::ctrpass::cec::envelope;
use ::ctrpass::cec::icon::Icon;
use ::ctrpass::export::{self, CecFolder};
use ::ctrpass::identity::{Identity, Seed};
use ::ctrpass::keys::{Keys, TitleKey};
use ::ctrpass::store::{self as rs_store, Mailbox, snapshot::mailbox_named};
use ::ctrpass::title::{self, Context, Values};
use pyo3::exceptions::{PyKeyError, PyRuntimeError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::PyBytes;
use serde_json::{Value, json};

fn err(e: impl std::fmt::Display) -> PyErr {
    PyValueError::new_err(e.to_string())
}

/// Turns a JSON value into the matching Python object.
fn to_py(py: Python<'_>, v: &Value) -> PyResult<PyObject> {
    let json = py.import("json")?;
    Ok(json.call_method1("loads", (v.to_string(),))?.unbind())
}

/// Turns a Python object of plain types into JSON.
fn from_py(obj: &Bound<'_, PyAny>) -> PyResult<Value> {
    let json = obj.py().import("json")?;
    let text: String = json.call_method1("dumps", (obj,))?.extract()?;
    serde_json::from_str(&text).map_err(err)
}

fn parse_keys(text: Option<&str>) -> PyResult<Option<Keys>> {
    text.map(|t| Keys::parse(t).map_err(err)).transpose()
}

fn title_key(b: &[u8]) -> PyResult<TitleKey> {
    b.try_into().map_err(|_| err("a title key is 32 bytes"))
}

fn mailbox(name: &str) -> PyResult<Mailbox> {
    mailbox_named(name).map_err(err)
}

fn now() -> Timestamp {
    let d = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    Timestamp::from_unix(d.as_secs(), d.subsec_millis() as u16)
}

#[pyclass(module = "ctrpass", frozen)]
#[derive(Clone)]
struct Container(RsContainer);

#[pymethods]
impl Container {
    #[new]
    fn new(data: &[u8]) -> PyResult<Self> {
        RsContainer::parse(data).map(Container).map_err(err)
    }

    fn __bytes__<'py>(&self, py: Python<'py>) -> Bound<'py, PyBytes> {
        PyBytes::new(py, self.0.as_bytes())
    }

    fn __len__(&self) -> usize {
        self.0.len()
    }

    #[getter]
    fn title(&self) -> u32 {
        self.0.title()
    }

    #[getter]
    fn message_id(&self) -> u64 {
        self.0.message_id()
    }

    #[getter]
    fn send_mode(&self) -> u8 {
        self.0.send_mode()
    }

    #[getter]
    fn send_count(&self) -> u8 {
        self.0.send_count()
    }

    /// When the message was made, as the sending console's clock read.
    #[getter]
    fn created(&self) -> String {
        self.0.created().to_string()
    }

    /// The text shown with the message, if it has one.
    #[getter]
    fn text(&self) -> Option<String> {
        title::message_text(&self.0)
    }

    /// The message's own 40x40 icon as RGB bytes, if it has one.
    #[getter]
    fn icon<'py>(&self, py: Python<'py>) -> Option<Bound<'py, PyBytes>> {
        let raw = self.0.ext_header(::ctrpass::cec::container::EXT_ICON)?;
        let icon = Icon::decode(40, raw).ok()?;
        Some(PyBytes::new(py, &icon.to_rgb().concat()))
    }

    #[getter]
    fn body<'py>(&self, py: Python<'py>) -> Bound<'py, PyBytes> {
        PyBytes::new(py, self.0.body())
    }

    fn verify(&self, key: &[u8]) -> PyResult<bool> {
        Ok(self.0.verify(&title_key(key)?))
    }

    fn __repr__(&self) -> String {
        format!(
            "<Container {:08X}/{:016x} {} bytes>",
            self.0.title(),
            self.0.message_id(),
            self.0.len()
        )
    }
}

/// Every message in a container or envelope file.
#[pyfunction]
fn read_messages(data: &[u8]) -> PyResult<Vec<Container>> {
    Ok(export::source::messages_in(data)
        .map_err(err)?
        .into_iter()
        .map(Container)
        .collect())
}

/// Checks the key file's text, returning the titles it signs for and whether it can read Miis.
#[pyfunction]
fn check_keys(py: Python<'_>, text: &str) -> PyResult<PyObject> {
    let k = Keys::parse(text).map_err(err)?;
    let titles: Vec<u32> = k.titles.keys().copied().collect();
    to_py(py, &json!({"titles": titles, "record": k.record.is_some()}))
}

/// The titles messages can be built for, with their fields.
#[pyfunction]
fn titles(py: Python<'_>) -> PyResult<PyObject> {
    let all: Vec<Value> = title::all()
        .iter()
        .map(|t| {
            json!({
                "title": t.id(),
                "name": t.name(),
                "send_mode": t.send_mode(),
                "fields": t.fields(),
            })
        })
        .collect();
    to_py(py, &Value::from(all))
}

fn find_title(id: u32) -> PyResult<&'static dyn title::Title> {
    title::by_id(id).ok_or_else(|| PyKeyError::new_err(format!("no fields are known for {id:08X}")))
}

/// The field values of a message, with any problems its title would refuse it for.
#[pyfunction]
#[pyo3(signature = (message, keys=None))]
fn read_fields(py: Python<'_>, message: &Container, keys: Option<&str>) -> PyResult<PyObject> {
    let keys = parse_keys(keys)?;
    let t = find_title(message.0.title())?;
    let ctx = Context {
        record_key: keys.as_ref().and_then(|k| k.record.as_ref()),
        identity: None,
    };
    let body = message.0.body();
    let values = t.read(body, &ctx).map_err(err)?;
    to_py(py, &json!({"values": values, "problems": t.problems(body)}))
}

/// Builds a signed message for `title_id` from field values, starting from `template` or a new body.
#[pyfunction]
#[pyo3(signature = (title_id, values, keys, template=None, seed=None, text=None))]
fn build_message(
    title_id: u32,
    values: &Bound<'_, PyAny>,
    keys: &str,
    template: Option<&Container>,
    seed: Option<&[u8]>,
    text: Option<&str>,
) -> PyResult<Container> {
    let t = find_title(title_id)?;
    let keys = Keys::parse(keys).map_err(err)?;
    let key = keys
        .title(title_id)
        .ok_or_else(|| err(format!("the key file has no titleKey{title_id:08X}")))?;
    let identity = seed
        .map(|s| -> PyResult<Identity> {
            let s: Seed = s.try_into().map_err(|_| err("a seed is 32 bytes"))?;
            Ok(Identity::from_seed(&s))
        })
        .transpose()?;
    let ctx = Context {
        record_key: keys.record.as_ref(),
        identity: identity.as_ref(),
    };
    let values: Values = match from_py(values)? {
        Value::Object(m) => m,
        _ => return Err(err("values must be a dict")),
    };
    let body = match template {
        Some(c) => c.0.body().to_vec(),
        None if identity.is_some() => t.create(&ctx).map_err(err)?,
        None => return Err(err("a new message needs the store seed")),
    };
    let body = t.write(&body, &values, &ctx).map_err(err)?;
    let problems = t.problems(&body);
    if !problems.is_empty() {
        return Err(err(format!(
            "{} would not keep this message: {}",
            t.name(),
            problems.join("; ")
        )));
    }
    title::message(t, &body, key, template.map(|c| &c.0), None, text)
        .map(Container)
        .map_err(err)
}

#[pyclass(module = "ctrpass", frozen)]
struct Store {
    path: PathBuf,
    inner: Mutex<Option<rs_store::Store>>,
}

impl Store {
    fn with<T>(
        &self,
        f: impl FnOnce(&mut rs_store::Store) -> Result<T, ::ctrpass::Error>,
    ) -> PyResult<T> {
        let mut g = self.inner.lock().unwrap();
        let s = g
            .as_mut()
            .ok_or_else(|| PyRuntimeError::new_err("the store is closed"))?;
        f(s).map_err(err)
    }
}

#[pymethods]
impl Store {
    /// Opens or creates the store at `path`, holding its lock until closed.
    #[new]
    fn new(path: PathBuf) -> PyResult<Self> {
        let s = rs_store::Store::open(&path).map_err(err)?;
        Ok(Store {
            path,
            inner: Mutex::new(Some(s)),
        })
    }

    #[getter]
    fn path(&self) -> PathBuf {
        self.path.clone()
    }

    fn close(&self) {
        self.inner.lock().unwrap().take();
    }

    /// Every title with its name, whether a key is held, whether it receives, and its message counts.
    fn titles(&self, py: Python<'_>) -> PyResult<PyObject> {
        let v = self.with(|s| {
            let receive = s.receive_titles();
            s.titles()
                .iter()
                .map(|&t| {
                    let count = |m| s.messages(Some(t), Some(m)).map(|v| v.len());
                    Ok(json!({
                        "title": t,
                        "name": s.title_name(t),
                        "key": s.title_key(t).is_some(),
                        "receive": receive.contains(&t),
                        "inbox": count(Mailbox::Inbox)?,
                        "outbox": count(Mailbox::Outbox)?,
                    }))
                })
                .collect::<Result<Vec<_>, ::ctrpass::Error>>()
        })?;
        to_py(py, &Value::from(v))
    }

    /// The messages held, as (box, Container), for one title or box when given.
    #[pyo3(signature = (title=None, r#box=None))]
    fn messages(
        &self,
        title: Option<u32>,
        r#box: Option<&str>,
    ) -> PyResult<Vec<(String, Container)>> {
        let m = r#box.map(mailbox).transpose()?;
        self.with(|s| {
            Ok(s.messages(title, m)?
                .into_iter()
                .map(|st| (st.mailbox.name().to_string(), Container(st.container)))
                .collect())
        })
    }

    #[pyo3(signature = (title, name=None, key=None, replace=false))]
    fn add_title(
        &self,
        title: u32,
        name: Option<&str>,
        key: Option<&[u8]>,
        replace: bool,
    ) -> PyResult<()> {
        let key = key.map(title_key).transpose()?;
        self.with(|s| s.add_title(title, name, key.as_ref(), replace))
    }

    /// Removes a title and its messages, returning how many messages went.
    fn remove_title(&self, title: u32) -> PyResult<usize> {
        self.with(|s| s.remove_title(title))
    }

    /// Files a message, returning how many older inbox messages were dropped to fit it.
    #[pyo3(signature = (message, r#box="outbox"))]
    fn add_message(&self, message: &Container, r#box: &str) -> PyResult<usize> {
        let m = mailbox(r#box)?;
        self.with(|s| {
            s.add_message(
                &message.0,
                m,
                None,
                Some(rs_store::Origin::Local("added in the app".into())),
            )
        })
    }

    /// Makes `message` the only outbox message of its title, returning how many it replaced.
    fn replace_outbox(&self, message: &Container) -> PyResult<usize> {
        self.with(|s| {
            s.replace_outbox(
                &message.0,
                None,
                Some(rs_store::Origin::Local("added in the app".into())),
            )
        })
    }

    fn remove_message(&self, title: u32, r#box: &str, message_id: u64) -> PyResult<bool> {
        let m = mailbox(r#box)?;
        self.with(|s| s.remove_message(title, m, message_id))
    }

    fn set_receive(&self, title: u32, on: bool) -> PyResult<()> {
        self.with(|s| s.set_receive(title, on))
    }

    #[getter]
    fn seed<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyBytes>> {
        let seed = self.with(|s| s.seed())?;
        Ok(PyBytes::new(py, &seed))
    }

    /// The identity this store exchanges as.
    fn identity(&self, py: Python<'_>) -> PyResult<PyObject> {
        let i = self.with(|s| s.identity())?;
        to_py(
            py,
            &json!({
                "mac": i.mac.iter().map(|b| format!("{b:02x}")).collect::<Vec<_>>().join(":"),
                "mii_id": i.mii_id,
            }),
        )
    }

    /// The whole store as JSON text, for the radio helper's load command.
    fn snapshot(&self) -> PyResult<String> {
        let snap = self.with(|s| s.snapshot())?;
        serde_json::to_string(&snap).map_err(err)
    }

    /// Files what the helper reported arriving from `peer`, returning the IDs filed and refused.
    fn file_received(
        &self,
        py: Python<'_>,
        messages: Vec<Vec<u8>>,
        peer: &str,
    ) -> PyResult<PyObject> {
        let mac: Vec<u8> = peer
            .split(':')
            .map(|p| u8::from_str_radix(p, 16))
            .collect::<Result<_, _>>()
            .map_err(|_| err("peer is not a MAC"))?;
        let mac: [u8; 6] = mac.try_into().map_err(|_| err("peer is not a MAC"))?;
        let cs = messages
            .iter()
            .map(|m| RsContainer::parse(m).map_err(err))
            .collect::<PyResult<Vec<_>>>()?;
        let mut by_title: std::collections::BTreeMap<u32, Vec<RsContainer>> = Default::default();
        for c in cs {
            by_title.entry(c.title()).or_default().push(c);
        }
        let (filed, refused) = self.with(|s| {
            let (mut filed, mut refused) = (Vec::new(), Vec::new());
            for cs in by_title.values() {
                let r = s.file_inbound(cs, mac)?;
                filed.extend(r.filed);
                refused.extend(r.refused);
            }
            Ok((filed, refused))
        })?;
        to_py(py, &json!({"filed": filed, "refused": refused}))
    }

    /// Replaces the peer book with the helper's, given as JSON text.
    fn set_peers(&self, text: &str) -> PyResult<()> {
        let v: Value = serde_json::from_str(text).map_err(err)?;
        self.with(|s| s.set_peers_json(&v))
    }

    /// Copies messages and title keys in from a save image, CEC folder or message file.
    #[pyo3(signature = (path, to=None))]
    fn import_from(&self, py: Python<'_>, path: PathBuf, to: Option<&str>) -> PyResult<PyObject> {
        let to = to.map(mailbox).transpose()?;
        let src = export::read(&path).map_err(err)?;
        let got = self.with(|s| export::import(s, &src, to))?;
        to_py(
            py,
            &json!({
                "added": got.added,
                "duplicates": got.duplicates,
                "refused": got.refused,
                "titles": got.titles,
                "key_conflicts": got.key_conflicts,
                "skipped": src.skipped,
            }),
        )
    }

    /// Delivers one box into a console-shaped CEC folder, returning what happened to each message.
    #[pyo3(signature = (dest, r#box="inbox", title=None, dry_run=false))]
    fn export_to(
        &self,
        py: Python<'_>,
        dest: PathBuf,
        r#box: &str,
        title: Option<u32>,
        dry_run: bool,
    ) -> PyResult<PyObject> {
        let m = mailbox(r#box)?;
        let folder = CecFolder::open(&dest).map_err(err)?;
        let mut messages = self.with(|s| s.messages(title, Some(m)))?;
        messages.sort_by_key(|st| st.container.created());
        let when = now();
        let mut out = Vec::new();
        for st in &messages {
            let d = folder.deliver(&st.container, when, dry_run).map_err(err)?;
            out.push(json!({
                "title": st.container.title(),
                "message_id": st.container.message_id(),
                "delivered": d.delivered(),
                "result": d.describe(),
            }));
        }
        to_py(py, &json!({"folder": folder.path(), "messages": out}))
    }

    fn __repr__(&self) -> String {
        format!("<Store {}>", self.path.display())
    }
}

/// Decodes a square image field of `size` pixels into RGB bytes.
#[pyfunction]
fn icon_rgb<'py>(py: Python<'py>, size: usize, data: &[u8]) -> PyResult<Bound<'py, PyBytes>> {
    let icon = Icon::decode(size, data).map_err(err)?;
    Ok(PyBytes::new(py, &icon.to_rgb().concat()))
}

/// Scales RGB pixels of any size into a square image field of `size` pixels.
#[pyfunction]
fn icon_from_rgb<'py>(
    py: Python<'py>,
    size: usize,
    width: usize,
    height: usize,
    rgb: &[u8],
) -> PyResult<Bound<'py, PyBytes>> {
    if width == 0 || height == 0 || rgb.len() != width * height * 3 {
        return Err(err("the pixels do not match the size"));
    }
    let pixels: Vec<[u8; 3]> = rgb.chunks_exact(3).map(|p| [p[0], p[1], p[2]]).collect();
    let icon = Icon::from_rgb(size, width, height, &pixels);
    Ok(PyBytes::new(py, &icon.encode()))
}

/// The signing key the key file's text holds for a title, if any.
#[pyfunction]
fn title_key_of<'py>(
    py: Python<'py>,
    keys: &str,
    title_id: u32,
) -> PyResult<Option<Bound<'py, PyBytes>>> {
    let k = Keys::parse(keys).map_err(err)?;
    Ok(k.title(title_id).map(|key| PyBytes::new(py, key)))
}

/// Wraps containers of one title in an envelope, as a file to share.
#[pyfunction]
fn envelope_of<'py>(py: Python<'py>, messages: Vec<Container>) -> PyResult<Bound<'py, PyBytes>> {
    let cs: Vec<RsContainer> = messages.into_iter().map(|c| c.0).collect();
    Ok(PyBytes::new(py, &envelope::build(&cs).map_err(err)?))
}

#[pymodule]
fn ctrpass(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<Container>()?;
    m.add_class::<Store>()?;
    m.add_function(wrap_pyfunction!(read_messages, m)?)?;
    m.add_function(wrap_pyfunction!(check_keys, m)?)?;
    m.add_function(wrap_pyfunction!(titles, m)?)?;
    m.add_function(wrap_pyfunction!(read_fields, m)?)?;
    m.add_function(wrap_pyfunction!(build_message, m)?)?;
    m.add_function(wrap_pyfunction!(envelope_of, m)?)?;
    m.add_function(wrap_pyfunction!(icon_rgb, m)?)?;
    m.add_function(wrap_pyfunction!(icon_from_rgb, m)?)?;
    m.add_function(wrap_pyfunction!(title_key_of, m)?)?;
    m.add("version", env!("CARGO_PKG_VERSION"))?;
    Ok(())
}
