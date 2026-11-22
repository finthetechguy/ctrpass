use std::collections::HashMap;
use std::path::Path;

use ctrpass::keys::Keys;
use ctrpass::radio::pcap::{LINKTYPE_IEEE802_11, LINKTYPE_RADIOTAP, Pcap};
use ctrpass::streetpass::ie::{self, Discovery, Kind};
use ctrpass::streetpass::linkkey::{self, Token};
use ctrpass::wifi::frame::{Header, Mac, TYPE_DATA};
use ctrpass::wifi::{ccmp, radiotap};
use serde_json::{Value, json};

use crate::{hex, mac_text};

fn discovery_json(d: &Discovery) -> Value {
    json!({
        "token": d.token.map(|t| hex(&t)),
        "channel": d.channel,
        "services": d.services.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
        "problem": d.problem,
    })
}

pub fn run(path: &Path, keys: Option<&Keys>) -> Result<(), String> {
    let data = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let pcap = Pcap::new(&data).map_err(|e| e.to_string())?;
    let linktype = pcap.linktype;
    if linktype != LINKTYPE_RADIOTAP && linktype != LINKTYPE_IEEE802_11 {
        return Err(format!("unsupported link type {linktype}"));
    }
    let mut tokens: HashMap<Mac, Token> = HashMap::new();
    let (mut verified, mut failed) = (0u32, 0u32);
    for packet in pcap {
        let packet = packet.map_err(|e| e.to_string())?;
        let frame = if linktype == LINKTYPE_RADIOTAP {
            match radiotap::parse(packet.data) {
                Ok((info, f)) if !info.bad_fcs => f,
                _ => continue,
            }
        } else {
            packet.data
        };
        let t = packet.timestamp_us as f64 / 1e6;
        if let Ok(Some((h, kind))) = ie::classify(frame) {
            let (name, detail) = match &kind {
                Kind::Probe(d) => ("probe", discovery_json(d)),
                Kind::Offer(d) => ("offer", discovery_json(d)),
                Kind::DirectedProbe => ("directed_probe", Value::Null),
                Kind::Accept => ("accept", Value::Null),
                Kind::LinkUp => ("link_up", Value::Null),
            };
            if let Kind::Probe(d) | Kind::Offer(d) = &kind {
                if let Some(token) = d.token {
                    tokens.insert(h.addr2, token);
                }
            }
            println!(
                "{}",
                json!({ "t": t, "frame": name, "from": mac_text(&h.addr2), "to": mac_text(&h.addr1), "detail": detail })
            );
            continue;
        }
        let Some(keys) = keys else { continue };
        let Ok(h) = Header::parse(frame) else {
            continue;
        };
        if h.ftype() != TYPE_DATA || !h.protected() {
            continue;
        }
        let host = h.addr3;
        let client = if h.addr2 == host { h.addr1 } else { h.addr2 };
        let (Some(host_token), Some(client_token)) = (tokens.get(&host), tokens.get(&client))
        else {
            continue;
        };
        let key = linkkey::derive(keys, (host_token, &host), (client_token, &client));
        match ccmp::decrypt(&key, frame) {
            Ok((pn, body)) => {
                verified += 1;
                println!(
                    "{}",
                    json!({
                        "t": t, "frame": "data", "from": mac_text(&h.addr2), "to": mac_text(&h.addr1),
                        "pn": pn, "len": body.len(), "head": hex(&body[..body.len().min(64)]),
                    })
                );
            }
            Err(_) => failed += 1,
        }
    }
    if keys.is_some() {
        eprintln!("{verified} data frames authenticated, {failed} failed");
    }
    Ok(())
}
