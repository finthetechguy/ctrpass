//! A helper for the desktop app: commands arrive as JSON lines on stdin, events leave on stdout.

use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, TryRecvError};
use std::time::Duration;

use ctrpass::exchange::{PassConfig, rotate_if_due, rotate_now, run_pass};
use ctrpass::keys::Keys;
use ctrpass::radio::linux::Mode;
use ctrpass::store::{Mailbox, Snapshot, Store};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::exchange::{RoleArg, event_json, report_json};
use crate::{found_json, hex, mac_text, open_radio};

#[derive(Debug, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum RoleName {
    Lead,
    Wait,
    Alternate,
}

#[derive(Debug, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum RadioName {
    Auto,
    Active,
    Mixed,
}

#[derive(Debug, Deserialize, PartialEq)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum Command {
    /// The key file's text, read by the app so this process never opens user paths.
    Keys {
        text: String,
    },
    /// The store's contents, which this process exchanges from a private copy of.
    Load {
        store: Snapshot,
    },
    Scan {
        #[serde(default = "default_channels")]
        channels: Vec<u8>,
        #[serde(default = "default_dwell")]
        dwell: f64,
    },
    Exchange {
        #[serde(default = "default_role")]
        role: RoleName,
        #[serde(default = "default_channels")]
        channels: Vec<u8>,
        #[serde(default = "default_window")]
        window: f64,
        /// Passes to run; without it, passes run until stopped.
        #[serde(default)]
        passes: Option<u32>,
        #[serde(default = "default_radio")]
        radio: RadioName,
        /// Seconds between changing the MAC and clearing the filter; 0 for never.
        #[serde(default = "default_rotate_every")]
        rotate_every: f64,
    },
    /// Change the MAC and clear the filter now, also while exchanging.
    Rotate,
    Stop,
    Quit,
}

fn default_channels() -> Vec<u8> {
    ctrpass::scan::CHANNELS.to_vec()
}

fn default_dwell() -> f64 {
    0.5
}

fn default_role() -> RoleName {
    RoleName::Lead
}

fn default_window() -> f64 {
    60.0
}

fn default_radio() -> RadioName {
    RadioName::Auto
}

fn default_rotate_every() -> f64 {
    ctrpass::store::peers::ROTATE_EVERY.as_secs_f64()
}

pub fn parse_command(line: &str) -> Result<Command, String> {
    serde_json::from_str(line).map_err(|e| format!("bad command: {e}"))
}

fn emit(v: Value) {
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{v}");
    let _ = out.flush();
}

fn error(message: impl std::fmt::Display) {
    emit(json!({ "event": "error", "message": message.to_string() }));
}

fn state(s: &str) {
    emit(json!({ "event": "state", "state": s }));
}

/// A private folder for the store copy, removed when dropped.
struct Scratch {
    path: PathBuf,
}

impl Scratch {
    fn new() -> std::io::Result<Self> {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let path =
            std::env::temp_dir().join(format!("ctrpass-serve-{}-{nanos}", std::process::id()));
        let mut b = std::fs::DirBuilder::new();
        #[cfg(unix)]
        std::os::unix::fs::DirBuilderExt::mode(&mut b, 0o700);
        b.create(&path)?;
        Ok(Scratch { path })
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

struct Helper {
    interface: String,
    force: bool,
    commands: Receiver<Command>,
    cancel: Arc<AtomicBool>,
    keys: Option<Keys>,
    /// The store copy; dropped before its folder.
    store: Option<Store>,
    scratch: Option<Scratch>,
}

enum Next {
    Continue,
    Quit,
}

/// What a command arriving during a long operation asks for.
enum Interrupt {
    End(Next),
    Rotate,
}

/// Takes a waiting command during a long operation.
fn interrupted(commands: &Receiver<Command>) -> Option<Interrupt> {
    match commands.try_recv() {
        Ok(Command::Quit) | Err(TryRecvError::Disconnected) => Some(Interrupt::End(Next::Quit)),
        Ok(Command::Stop) => Some(Interrupt::End(Next::Continue)),
        Ok(Command::Rotate) => Some(Interrupt::Rotate),
        Ok(other) => {
            error(format!("busy, ignored {other:?}"));
            None
        }
        Err(TryRecvError::Empty) => None,
    }
}

/// Reports a new MAC with the cleared peer book, which the app keeps in its own store.
fn emit_rotated(store: &Store, mac: &ctrpass::wifi::frame::Mac) -> Result<(), String> {
    emit(json!({
        "event": "rotated",
        "mac": mac_text(mac),
        "peers": store.peers_json().map_err(|e| e.to_string())?,
    }));
    Ok(())
}

impl Helper {
    fn rotate(&mut self) -> Result<(), String> {
        let store = self.store.as_ref().ok_or("no store loaded")?;
        let mac = rotate_now(store).map_err(|e| e.to_string())?;
        emit_rotated(store, &mac)
    }

    fn load(&mut self, snap: &Snapshot) -> Result<(), String> {
        self.store = None;
        self.scratch = None;
        let scratch = Scratch::new().map_err(|e| e.to_string())?;
        let mut store = Store::open(&scratch.path).map_err(|e| e.to_string())?;
        store.restore(snap).map_err(|e| e.to_string())?;
        let mac = store.identity().map_err(|e| e.to_string())?.mac;
        emit(json!({
            "event": "loaded",
            "mac": mac_text(&mac),
            "titles": store.titles().len(),
            "messages": snap.messages.len(),
        }));
        self.store = Some(store);
        self.scratch = Some(scratch);
        Ok(())
    }

    fn scan(&mut self, channels: &[u8], dwell: f64) -> Result<Next, String> {
        let mut radio = open_radio(&self.interface, None, self.force, Some(Mode::Listen))?;
        state("scanning");
        loop {
            ctrpass::scan::scan(
                &mut radio,
                channels,
                Duration::from_secs_f64(dwell),
                1,
                &self.cancel,
                |f| emit(json!({ "event": "found", "console": found_json(f) })),
            )
            .map_err(|e| e.to_string())?;
            if self.cancel.swap(false, Ordering::Relaxed) {
                match interrupted(&self.commands) {
                    Some(Interrupt::Rotate) => self.rotate()?,
                    Some(Interrupt::End(n)) => return Ok(n),
                    None => return Ok(Next::Continue),
                }
            }
        }
    }

    fn exchange(
        &mut self,
        role: RoleName,
        channels: Vec<u8>,
        window: f64,
        passes: Option<u32>,
        radio_name: RadioName,
        rotate_every: f64,
    ) -> Result<Next, String> {
        let keys = self.keys.as_ref().ok_or("no keys loaded")?;
        let store = self.store.as_mut().ok_or("no store loaded")?;
        let mode = match radio_name {
            RadioName::Auto => None,
            RadioName::Active => Some(Mode::Active),
            RadioName::Mixed => Some(Mode::Mixed),
        };
        let role = match role {
            RoleName::Lead => RoleArg::Lead,
            RoleName::Wait => RoleArg::Wait,
            RoleName::Alternate => RoleArg::Alternate,
        };
        let every = crate::exchange::rotate_interval(rotate_every / 3600.0);
        let mut pass = 0;
        // Each turn opens the radio with the store's current MAC, and a rotation starts a new turn.
        'radio: loop {
            let mac = store.identity().map_err(|e| e.to_string())?.mac;
            let mut radio = open_radio(&self.interface, Some(mac), self.force, mode)?;
            let mut channels = channels.clone();
            if let Some(fixed) = radio.fixed_channel() {
                channels = vec![fixed];
            }
            emit(json!({
                "event": "radio",
                "mac": mac_text(&mac),
                "mixed": radio.fixed_channel().is_some(),
                "channels": channels,
            }));
            state("exchanging");
            while passes.is_none_or(|n| pass < n) {
                if let Some(mac) = rotate_if_due(store, every).map_err(|e| e.to_string())? {
                    emit_rotated(store, &mac)?;
                    continue 'radio;
                }
                let config = PassConfig {
                    role: role.for_pass(pass),
                    channels: channels.clone(),
                    window: Duration::from_secs_f64(window),
                };
                let report = run_pass(&mut radio, store, keys, &config, &self.cancel, |e| {
                    if let Some(v) = event_json(e) {
                        emit(v);
                    }
                })
                .map_err(|e| e.to_string())?;
                if report.peer.is_some() {
                    let mut v = report_json(pass, config.role, &report);
                    let held = store
                        .messages(None, Some(Mailbox::Inbox))
                        .map_err(|e| e.to_string())?;
                    // The app files these in its own store, since this process never writes there.
                    let arrived: Vec<Value> = report
                        .received
                        .iter()
                        .filter_map(|&(_, id)| {
                            held.iter()
                                .find(|s| s.container.message_id() == id)
                                .map(|s| hex(s.container.as_bytes()))
                        })
                        .map(Value::from)
                        .collect();
                    v["messages"] = Value::from(arrived);
                    v["peers"] = store.peers_json().map_err(|e| e.to_string())?;
                    emit(v);
                }
                pass += 1;
                let cancelled = self.cancel.swap(false, Ordering::Relaxed);
                match interrupted(&self.commands) {
                    Some(Interrupt::Rotate) => {
                        let mac = rotate_now(store).map_err(|e| e.to_string())?;
                        emit_rotated(store, &mac)?;
                        continue 'radio;
                    }
                    Some(Interrupt::End(n)) => return Ok(n),
                    None if cancelled => return Ok(Next::Continue),
                    None => {}
                }
            }
            return Ok(Next::Continue);
        }
    }
}

pub fn run(interface: &str, force: bool) -> Result<(), String> {
    let (tx, commands) = mpsc::channel();
    let cancel = Arc::new(AtomicBool::new(false));
    let reader_cancel = cancel.clone();
    std::thread::spawn(move || {
        for line in std::io::stdin().lock().lines() {
            let Ok(line) = line else { break };
            if line.trim().is_empty() {
                continue;
            }
            match parse_command(&line) {
                Ok(c) => {
                    if matches!(c, Command::Stop | Command::Quit | Command::Rotate) {
                        reader_cancel.store(true, Ordering::Relaxed);
                    }
                    if tx.send(c).is_err() {
                        break;
                    }
                }
                Err(e) => error(e),
            }
        }
        reader_cancel.store(true, Ordering::Relaxed);
    });

    emit(json!({ "event": "hello", "version": env!("CARGO_PKG_VERSION"), "interface": interface }));
    state("idle");
    let mut helper = Helper {
        interface: interface.into(),
        force,
        commands,
        cancel,
        keys: None,
        store: None,
        scratch: None,
    };
    loop {
        let cmd = match helper.commands.recv_timeout(Duration::from_secs(1)) {
            Ok(c) => c,
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => break,
        };
        helper.cancel.store(false, Ordering::Relaxed);
        let result = match cmd {
            Command::Keys { text } => match Keys::parse(&text) {
                Ok(k) => {
                    let titles: Vec<String> = k.titles.keys().map(|t| format!("{t:08X}")).collect();
                    emit(
                        json!({ "event": "keys", "titles": titles, "record": k.record.is_some() }),
                    );
                    helper.keys = Some(k);
                    Ok(Next::Continue)
                }
                Err(e) => Err(e.to_string()),
            },
            Command::Load { store } => helper.load(&store).map(|()| Next::Continue),
            Command::Scan { channels, dwell } => helper.scan(&channels, dwell),
            Command::Exchange {
                role,
                channels,
                window,
                passes,
                radio,
                rotate_every,
            } => helper.exchange(role, channels, window, passes, radio, rotate_every),
            Command::Rotate => helper.rotate().map(|()| Next::Continue),
            Command::Stop => Ok(Next::Continue),
            Command::Quit => Ok(Next::Quit),
        };
        let next = result.unwrap_or_else(|e| {
            error(e);
            Next::Continue
        });
        if matches!(next, Next::Quit) {
            break;
        }
        state("idle");
    }
    helper.store = None;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_commands() {
        assert_eq!(
            parse_command(r#"{"cmd":"scan"}"#).unwrap(),
            Command::Scan {
                channels: default_channels(),
                dwell: 0.5
            }
        );
        assert_eq!(
            parse_command(r#"{"cmd":"exchange","role":"wait","passes":1}"#).unwrap(),
            Command::Exchange {
                role: RoleName::Wait,
                channels: default_channels(),
                window: 60.0,
                passes: Some(1),
                radio: RadioName::Auto,
                rotate_every: 8.0 * 3600.0,
            }
        );
        assert_eq!(
            parse_command(r#"{"cmd":"rotate"}"#).unwrap(),
            Command::Rotate
        );
        let load = r#"{"cmd":"load","store":{"seed":"00","titles":[],"messages":[]}}"#;
        assert!(matches!(parse_command(load), Ok(Command::Load { .. })));
        assert!(parse_command(r#"{"cmd":"exchange","role":"both"}"#).is_err());
        assert!(parse_command(r#"{"cmd":"format_disk"}"#).is_err());
        assert!(parse_command("not json").is_err());
    }

    #[test]
    fn loads_a_snapshot() {
        let src = tempfile::tempdir().unwrap();
        let snap = Store::open(src.path()).unwrap().snapshot().unwrap();
        let (_tx, commands) = mpsc::channel();
        let mut h = Helper {
            interface: "none".into(),
            force: false,
            commands,
            cancel: Arc::new(AtomicBool::new(false)),
            keys: None,
            store: None,
            scratch: None,
        };
        h.load(&snap).unwrap();
        let dir = h.scratch.as_ref().unwrap().path.clone();
        assert!(dir.join("CEC").is_dir());
        assert_eq!(
            h.store.as_ref().unwrap().snapshot().unwrap().seed,
            snap.seed
        );
        assert!(
            h.exchange(RoleName::Lead, vec![1], 1.0, Some(1), RadioName::Auto, 0.0)
                .is_err()
        );
        let before = h.store.as_ref().unwrap().identity().unwrap().mac;
        h.rotate().unwrap();
        assert_ne!(h.store.as_ref().unwrap().identity().unwrap().mac, before);
        h.store = None;
        h.scratch = None;
        assert!(!dir.exists());
    }
}
