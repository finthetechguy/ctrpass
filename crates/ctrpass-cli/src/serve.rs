//! A helper for the desktop app: commands arrive as JSON lines on stdin, events leave on stdout.

use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, TryRecvError};
use std::time::Duration;

use ctrpass::exchange::{PassConfig, run_pass};
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
    },
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

/// Takes a waiting command during a long operation, returning whether to stop it.
fn interrupted(commands: &Receiver<Command>) -> Option<Next> {
    match commands.try_recv() {
        Ok(Command::Quit) | Err(TryRecvError::Disconnected) => Some(Next::Quit),
        Ok(Command::Stop) => Some(Next::Continue),
        Ok(other) => {
            error(format!("busy, ignored {other:?}"));
            None
        }
        Err(TryRecvError::Empty) => None,
    }
}

impl Helper {
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
                return Ok(interrupted(&self.commands).unwrap_or(Next::Continue));
            }
        }
    }

    fn exchange(
        &mut self,
        role: RoleName,
        mut channels: Vec<u8>,
        window: f64,
        passes: Option<u32>,
        radio_name: RadioName,
    ) -> Result<Next, String> {
        let keys = self.keys.as_ref().ok_or("no keys loaded")?;
        let store = self.store.as_mut().ok_or("no store loaded")?;
        let mac = store.identity().map_err(|e| e.to_string())?.mac;
        let mode = match radio_name {
            RadioName::Auto => None,
            RadioName::Active => Some(Mode::Active),
            RadioName::Mixed => Some(Mode::Mixed),
        };
        let mut radio = open_radio(&self.interface, Some(mac), self.force, mode)?;
        if let Some(fixed) = radio.fixed_channel() {
            channels = vec![fixed];
        }
        emit(json!({
            "event": "radio",
            "mac": mac_text(&mac),
            "mixed": radio.fixed_channel().is_some(),
            "channels": channels,
        }));
        let role = match role {
            RoleName::Lead => RoleArg::Lead,
            RoleName::Wait => RoleArg::Wait,
            RoleName::Alternate => RoleArg::Alternate,
        };
        state("exchanging");
        let mut pass = 0;
        while passes.is_none_or(|n| pass < n) {
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
            if self.cancel.swap(false, Ordering::Relaxed) {
                return Ok(interrupted(&self.commands).unwrap_or(Next::Continue));
            }
            if let Some(n) = interrupted(&self.commands) {
                return Ok(n);
            }
        }
        Ok(Next::Continue)
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
                    if matches!(c, Command::Stop | Command::Quit) {
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
            } => helper.exchange(role, channels, window, passes, radio),
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
            }
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
            h.exchange(RoleName::Lead, vec![1], 1.0, Some(1), RadioName::Auto)
                .is_err()
        );
        h.store = None;
        h.scratch = None;
        assert!(!dir.exists());
    }
}
