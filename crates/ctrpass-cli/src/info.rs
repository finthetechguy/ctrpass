use std::path::Path;

use ctrpass::cec::container::{EXT_INFO_TEXT, EXT_SERVICE};
use ctrpass::cec::object::{self, kind};
use ctrpass::cec::{Container, envelope};
use ctrpass::keys::Keys;
use ctrpass::streetpass::service;
use serde_json::{Value, json};

use crate::hex;

fn utf16(data: &[u8]) -> String {
    let units: Vec<u16> = data
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .take_while(|&u| u != 0)
        .collect();
    String::from_utf16_lossy(&units)
}

fn container_json(c: &Container, keys: Option<&Keys>) -> Value {
    let signature = match keys.and_then(|k| k.title(c.title())) {
        Some(key) if c.verify(key) => "valid",
        Some(_) => "invalid",
        None => "unchecked",
    };
    let ext: Vec<Value> = c
        .ext_headers()
        .into_iter()
        .map(|(k, d)| match k {
            EXT_INFO_TEXT => json!({ "type": k, "text": utf16(d) }),
            EXT_SERVICE => json!({
                "type": k,
                "service": service::decode(d).0.first().map(|s| s.to_string()),
            }),
            _ => json!({ "type": k, "size": d.len() }),
        })
        .collect();
    json!({
        "title": format!("{:08X}", c.title()),
        "message_id": format!("{:016x}", c.message_id()),
        "size": c.len(),
        "body_size": c.body().len(),
        "send_mode": c.send_mode(),
        "flags": c.flags(),
        "send_count": c.send_count(),
        "forward_count": c.forward_count(),
        "created": c.created().to_string(),
        "ext_headers": ext,
        "signature": signature,
    })
}

fn object_json(obj: &[u8]) -> Value {
    let k = object::kind_of(obj).unwrap();
    let p = object::payload(obj);
    let detail = match k {
        kind::OFFER | kind::VERDICT | kind::VERDICT_REPLY | kind::OFFER_REPLY => {
            match object::parse_list(p) {
                Ok(entries) => json!(
                    entries
                        .iter()
                        .map(|e| json!({
                            "title": format!("{:08X}", e.title), "mode": e.mode, "flags": e.flags,
                            "code": e.code, "size": e.size, "count": e.count,
                        }))
                        .collect::<Vec<_>>()
                ),
                Err(e) => json!({ "error": e.to_string() }),
            }
        }
        kind::MESSAGES => match object::parse_message_list(p) {
            Ok(list) => json!(
                list.iter()
                    .map(|(t, size)| json!({ "title": format!("{t:08X}"), "size": size }))
                    .collect::<Vec<_>>()
            ),
            Err(e) => json!({ "error": e.to_string() }),
        },
        _ => json!(hex(p)),
    };
    json!({ "object": format!("{k:#04x}"), "size": obj.len(), "detail": detail })
}

pub fn run(path: &Path, keys: Option<&Keys>) -> Result<(), String> {
    let data = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let out = if object::kind_of(&data).is_some() {
        object_json(&data)
    } else {
        let cs = envelope::containers(&data).map_err(|e| e.to_string())?;
        let list: Vec<Value> = cs.iter().map(|c| container_json(c, keys)).collect();
        if envelope::title(&data).is_some() {
            json!({ "envelope": list })
        } else {
            list.into_iter().next().unwrap()
        }
    };
    println!("{}", serde_json::to_string_pretty(&out).unwrap());
    Ok(())
}
