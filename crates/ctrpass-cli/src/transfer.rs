use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use ctrpass::cec::container::Timestamp;
use ctrpass::export::{self, CecFolder};
use ctrpass::keys;
use ctrpass::store::{Mailbox, Store};
use serde_json::{Value, json};

use crate::store::BoxArg;

/// Data folders of emulators that keep the CEC system save as a plain folder.
const EMULATORS: [(&str, &str); 4] = [
    ("azahar-emu", "Azahar"),
    ("azaharplus-emu", "AzaharPlus"),
    ("citra-emu", "Citra"),
    ("lime3ds-emu", "Lime3DS"),
];
const EMULATOR_PROCESSES: [&str; 8] = [
    "azahar",
    "azahar-qt",
    "azaharplus",
    "citra",
    "citra-qt",
    "lime3ds",
    "lime3ds-qt",
    "azahar-room",
];

fn e(e: ctrpass::Error) -> String {
    e.to_string()
}

fn home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

/// Every installed emulator with a CEC folder, as (name, folder).
pub fn emulators() -> Vec<(String, PathBuf)> {
    let mut bases = Vec::new();
    if let Some(d) = std::env::var_os("XDG_DATA_HOME") {
        bases.push((PathBuf::from(d), ""));
    } else if let Some(h) = home() {
        bases.push((h.join(".local/share"), ""));
    }
    if let Some(Ok(apps)) = home().map(|h| fs::read_dir(h.join(".var/app"))) {
        let mut apps: Vec<_> = apps
            .filter_map(Result::ok)
            .map(|a| a.path().join("data"))
            .collect();
        apps.sort();
        bases.extend(apps.into_iter().map(|a| (a, " (Flatpak)")));
    }
    let mut out: Vec<(String, PathBuf)> = Vec::new();
    for (base, how) in bases {
        for (dir, name) in EMULATORS {
            if let Ok(f) = CecFolder::open(&base.join(dir)) {
                if !out.iter().any(|(_, p)| p == f.path()) {
                    out.push((format!("{name}{how}"), f.path().to_path_buf()));
                }
            }
        }
    }
    out
}

/// The name of a running emulator, which would overwrite what is written into its folder.
fn emulator_running() -> Option<String> {
    for p in fs::read_dir("/proc").ok()?.filter_map(Result::ok) {
        if let Ok(comm) = fs::read_to_string(p.path().join("comm")) {
            let comm = comm.trim().to_lowercase();
            if EMULATOR_PROCESSES.contains(&comm.as_str()) {
                return Some(comm);
            }
        }
    }
    None
}

pub fn import(store: &Path, from: &Path, to: Option<BoxArg>, dry_run: bool) -> Result<(), String> {
    let src = export::read(from).map_err(|err| format!("{}: {err}", from.display()))?;
    let skipped: Vec<Value> = src
        .skipped
        .iter()
        .map(|(p, why)| json!({"path": p, "reason": why}))
        .collect();
    let titles: Vec<Value> = src
        .titles
        .iter()
        .map(|t| {
            json!({
                "title": format!("{:08X}", t.title),
                "name": t.name,
                "key": t.key.is_some(),
                "messages": src.messages.iter().filter(|f| f.container.title() == t.title).count(),
            })
        })
        .collect();
    if dry_run {
        let found: Vec<Value> = src
            .messages
            .iter()
            .map(|f| {
                json!({
                    "title": format!("{:08X}", f.container.title()),
                    "id": format!("{:016x}", f.container.message_id()),
                    "box": f.mailbox.map(|m| m.name()),
                    "path": f.path,
                })
            })
            .collect();
        let out = json!({"titles": titles, "messages": found, "skipped": skipped});
        println!("{}", serde_json::to_string_pretty(&out).unwrap());
        return Ok(());
    }
    let mut store = Store::open(store).map_err(e)?;
    let got = export::import(&mut store, &src, to.map(Mailbox::from)).map_err(e)?;
    let refused: Vec<Value> = got
        .refused
        .iter()
        .map(|(p, why)| json!({"path": p, "reason": why}))
        .collect();
    let out = json!({
        "titles": titles,
        "added": got.added,
        "duplicates": got.duplicates,
        "refused": refused,
        "key_conflicts": got.key_conflicts.iter().map(|t| format!("{t:08X}")).collect::<Vec<_>>(),
        "skipped": skipped,
    });
    println!("{}", serde_json::to_string_pretty(&out).unwrap());
    Ok(())
}

pub struct ExportArgs<'a> {
    pub dest: Option<&'a Path>,
    pub mailbox: Mailbox,
    pub title: Option<u32>,
    pub raw: bool,
    pub dry_run: bool,
    pub force: bool,
}

pub fn export(store: &Path, args: &ExportArgs) -> Result<(), String> {
    let store = Store::open(store).map_err(e)?;
    let mut messages: Vec<_> = store
        .messages(args.title, Some(args.mailbox))
        .map_err(e)?
        .into_iter()
        .map(|s| s.container)
        .collect();
    messages.sort_by_key(|c| c.created());

    if args.raw {
        let dest = args.dest.ok_or("--raw needs a folder to write into")?;
        if args.dry_run {
            println!("{}", json!({"would_write": messages.len()}));
            return Ok(());
        }
        let paths = export::folder::write_raw(&messages, dest).map_err(e)?;
        let paths: Vec<String> = paths.iter().map(|p| p.display().to_string()).collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({"written": paths})).unwrap()
        );
        return Ok(());
    }

    let folder = match args.dest {
        Some(d) => CecFolder::open(d).map_err(e)?,
        None => {
            let found = emulators();
            match found.as_slice() {
                [] => return Err("no emulator CEC folder found; name one".into()),
                [(_, p)] => CecFolder::open(p).map_err(e)?,
                _ => {
                    let names: Vec<String> = found
                        .iter()
                        .map(|(n, p)| format!("{n}: {}", p.display()))
                        .collect();
                    return Err(format!(
                        "more than one emulator found; name one of\n  {}",
                        names.join("\n  ")
                    ));
                }
            }
        }
    };
    if !args.dry_run && !args.force {
        if let Some(name) = emulator_running() {
            return Err(format!(
                "{name} is running and would overwrite the folder when it exits; close it or pass --force"
            ));
        }
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let when = Timestamp::from_unix(now.as_secs(), now.subsec_millis() as u16);
    let mut results = Vec::new();
    for c in &messages {
        let d = folder.deliver(c, when, args.dry_run).map_err(e)?;
        results.push(json!({
            "title": format!("{:08X}", c.title()),
            "id": format!("{:016x}", c.message_id()),
            "delivered": d.delivered(),
            "result": d.describe(),
        }));
    }
    let delivered = results.iter().filter(|r| r["delivered"] == true).count();
    let out = json!({
        "folder": folder.path().display().to_string(),
        "dry_run": args.dry_run,
        "delivered": delivered,
        "messages": results,
    });
    println!("{}", serde_json::to_string_pretty(&out).unwrap());
    Ok(())
}

pub fn default_key_path() -> Result<PathBuf, String> {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| home().map(|h| h.join(".config")))
        .map(|d| d.join("ctrpass/keys"))
        .ok_or_else(|| "no key file: pass --keys".into())
}

/// Replaces `path` with `text`, readable only by its owner.
fn write_private(path: &Path, text: &str) -> Result<(), String> {
    let err = |err: std::io::Error| format!("{}: {err}", path.display());
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        let mut b = fs::DirBuilder::new();
        b.recursive(true);
        #[cfg(unix)]
        std::os::unix::fs::DirBuilderExt::mode(&mut b, 0o700);
        b.create(dir).map_err(err)?;
    }
    let tmp = path.with_extension("tmp");
    let mut o = fs::OpenOptions::new();
    o.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut o, 0o600);
    let mut f = o.open(&tmp).map_err(err)?;
    f.write_all(text.as_bytes()).map_err(err)?;
    f.sync_all().map_err(err)?;
    fs::rename(&tmp, path).map_err(err)
}

pub fn extract_keys(
    out: &Path,
    code: Option<&Path>,
    offset: Option<usize>,
    dumps: &[PathBuf],
) -> Result<(), String> {
    if code.is_none() && dumps.is_empty() {
        return Err("name a module with --module or a CEC dump with --titles".into());
    }
    let mut entries = Vec::new();
    let mut report = Vec::new();
    if let Some(path) = code {
        let data = fs::read(path).map_err(|err| format!("{}: {err}", path.display()))?;
        let key = keys::find_hmac_key(&data, offset)
            .map_err(|err| format!("{}: {err}", path.display()))?;
        entries.push((keys::hmac_entry().to_string(), key.to_vec()));
        report.push(json!({"entry": keys::hmac_entry()}));
    }
    for path in dumps {
        let src = export::read(path).map_err(|err| format!("{}: {err}", path.display()))?;
        let mut found = 0;
        for t in &src.titles {
            if let Some(key) = t.key {
                found += 1;
                let name = keys::title_entry(t.title);
                entries.retain(|(n, _)| *n != name);
                entries.push((name.clone(), key.to_vec()));
                report.push(json!({"entry": name, "name": t.name}));
            }
        }
        if found == 0 {
            return Err(format!("{} holds no title keys", path.display()));
        }
    }
    let old = match fs::read_to_string(out) {
        Ok(t) => t,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(err) => return Err(format!("{}: {err}", out.display())),
    };
    write_private(out, &keys::set_entries(&old, &entries))?;
    let out = json!({"file": out.display().to_string(), "written": report});
    println!("{}", serde_json::to_string_pretty(&out).unwrap());
    Ok(())
}
