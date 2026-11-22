use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use clap::ValueEnum;
use ctrpass::exchange::{PassConfig, Report, run_pass};
use ctrpass::keys::Keys;
use ctrpass::link::{Event, Role};
use ctrpass::radio::Radio;
use ctrpass::store::Store;
use serde_json::{Value, json};

use crate::mac_text;

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum RoleArg {
    Lead,
    Wait,
    /// Switch roles every pass.
    Alternate,
}

impl RoleArg {
    pub(crate) fn for_pass(self, pass: u32) -> Role {
        match self {
            RoleArg::Lead => Role::Lead,
            RoleArg::Wait => Role::Wait,
            RoleArg::Alternate if pass % 2 == 0 => Role::Lead,
            RoleArg::Alternate => Role::Wait,
        }
    }
}

pub(crate) fn role_name(role: Role) -> &'static str {
    match role {
        Role::Lead => "lead",
        Role::Wait => "wait",
    }
}

pub(crate) fn event_json(e: &Event) -> Option<Value> {
    Some(match e {
        Event::Engaged { peer } => json!({"event": "engaged", "peer": mac_text(peer)}),
        Event::Declined { peer } => json!({"event": "declined", "peer": mac_text(peer)}),
        Event::LinkUp { peer } => json!({"event": "link_up", "peer": mac_text(peer)}),
        Event::Abandoned { peer, reason } => {
            json!({"event": "abandoned", "peer": mac_text(peer), "reason": reason})
        }
        Event::Finished { peer } => json!({"event": "finished", "peer": mac_text(peer)}),
        Event::Session(_) => return None,
    })
}

fn ids(list: &[(u32, u64)]) -> Vec<Value> {
    list.iter()
        .map(|(t, id)| json!({"title": format!("{t:08X}"), "id": format!("{id:016x}")}))
        .collect()
}

pub(crate) fn report_json(pass: u32, role: Role, r: &Report) -> Value {
    json!({
        "event": "pass",
        "pass": pass,
        "role": role_name(role),
        "peer": r.peer.map(|m| mac_text(&m)),
        "sent": ids(&r.sent),
        "refused": r.refused.iter().map(|t| format!("{t:08X}")).collect::<Vec<_>>(),
        "received": ids(&r.received),
        "not_filed": r.not_filed.iter()
            .map(|(t, why)| json!({"title": format!("{t:08X}"), "reason": why}))
            .collect::<Vec<_>>(),
    })
}

pub struct Options {
    pub role: RoleArg,
    pub channels: Vec<u8>,
    pub window: Duration,
    pub passes: Option<u32>,
}

pub fn run(
    radio: &mut dyn Radio,
    store: &mut Store,
    keys: &Keys,
    opts: &Options,
    stop: &AtomicBool,
) -> Result<(), String> {
    let mut exchanged = 0;
    let mut pass = 0;
    while opts.passes.is_none_or(|n| pass < n) && !stop.load(Ordering::Relaxed) {
        let role = opts.role.for_pass(pass);
        let config = PassConfig {
            role,
            channels: opts.channels.clone(),
            window: opts.window,
        };
        let report = run_pass(radio, store, keys, &config, stop, |e| {
            if let Some(v) = event_json(e) {
                println!("{v}");
            }
        })
        .map_err(|e| e.to_string())?;
        if report.peer.is_some() {
            println!("{}", report_json(pass, role, &report));
        }
        if !report.sent.is_empty() || !report.received.is_empty() {
            exchanged += 1;
        }
        pass += 1;
    }
    eprintln!("{exchanged} exchange(s) in {pass} pass(es)");
    Ok(())
}
