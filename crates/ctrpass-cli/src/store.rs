use std::path::{Path, PathBuf};

use clap::{Subcommand, ValueEnum};
use ctrpass::cec::envelope;
use ctrpass::keys::Keys;
use ctrpass::store::{Mailbox, Origin, Store};
use serde_json::json;

use crate::{hex, mac_text};

#[derive(Clone, Copy, ValueEnum)]
pub enum BoxArg {
    Inbox,
    Outbox,
}

impl From<BoxArg> for Mailbox {
    fn from(b: BoxArg) -> Self {
        match b {
            BoxArg::Inbox => Mailbox::Inbox,
            BoxArg::Outbox => Mailbox::Outbox,
        }
    }
}

#[derive(Clone, Copy, ValueEnum)]
pub enum Switch {
    On,
    Off,
}

fn parse_hex<T: TryFrom<u64>>(s: &str) -> Result<T, String> {
    let digits = s.trim_start_matches("0x");
    u64::from_str_radix(digits, 16)
        .ok()
        .and_then(|v| T::try_from(v).ok())
        .ok_or_else(|| format!("{s:?} is not a hex number in range"))
}

#[derive(Subcommand)]
pub enum Action {
    /// Print every title and its messages as JSON.
    List,
    /// File the messages in a CEC message or envelope file.
    Add {
        file: PathBuf,
        #[arg(long = "box", value_enum, default_value_t = BoxArg::Outbox)]
        mailbox: BoxArg,
        /// Title name to record.
        #[arg(long)]
        name: Option<String>,
        /// Remove the title's other outbox messages.
        #[arg(long)]
        replace: bool,
    },
    /// Remove a title, or one of its messages when a message ID is given.
    Remove {
        #[arg(value_parser = parse_hex::<u32>)]
        title: u32,
        #[arg(value_parser = parse_hex::<u64>)]
        message: Option<u64>,
        #[arg(long = "box", value_enum, default_value_t = BoxArg::Outbox)]
        mailbox: BoxArg,
    },
    /// Turn receiving on or off for a title, adding the title if needed.
    Receive {
        #[arg(value_parser = parse_hex::<u32>)]
        title: u32,
        #[arg(value_enum)]
        state: Switch,
    },
    /// Print the identity the store presents.
    Identity {
        /// Replace it with a new random one first.
        #[arg(long)]
        new: bool,
    },
}

/// `$CTRPASS_STORE`, else the per-user data folder.
pub fn default_path() -> Result<PathBuf, String> {
    if let Some(p) = std::env::var_os("CTRPASS_STORE") {
        return Ok(p.into());
    }
    let data = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| Path::new(&h).join(".local/share")))
        .ok_or("no store folder: pass --store or set CTRPASS_STORE")?;
    Ok(data.join("ctrpass"))
}

fn listing(store: &Store) -> Result<serde_json::Value, ctrpass::Error> {
    let receive = store.receive_titles();
    let mut titles = Vec::new();
    for &t in store.titles() {
        let mut boxes = serde_json::Map::new();
        for mailbox in Mailbox::ALL {
            let held = store.messages(Some(t), Some(mailbox))?;
            let items: Vec<_> = held
                .iter()
                .map(|s| {
                    let id = s.container.message_id();
                    let origin = store.origin(mailbox, id).map(|p| match &p.origin {
                        Origin::Peer(mac) => json!({"peer": mac_text(mac), "when": p.when}),
                        Origin::Local(note) => json!({"local": note, "when": p.when}),
                    });
                    json!({
                        "id": format!("{id:016x}"),
                        "size": s.container.len(),
                        "verified": store.verify(&s.container),
                        "origin": origin,
                    })
                })
                .collect();
            boxes.insert(mailbox.name().into(), items.into());
        }
        titles.push(json!({
            "title": format!("{t:08X}"),
            "name": store.title_name(t),
            "key": store.title_key(t).is_some(),
            "receive": receive.contains(&t),
            "messages": boxes,
        }));
    }
    Ok(json!({ "root": store.root(), "titles": titles }))
}

pub fn run(path: &Path, action: Action, keys: Option<&Keys>) -> Result<(), String> {
    let mut store = Store::open(path).map_err(|e| e.to_string())?;
    for d in store.damaged() {
        eprintln!(
            "ctrpass: moved {} to {} ({})",
            d.file.display(),
            d.moved_to.display(),
            d.reason
        );
    }
    let e = |e: ctrpass::Error| e.to_string();
    match action {
        Action::List => println!(
            "{}",
            serde_json::to_string_pretty(&listing(&store).map_err(e)?).unwrap()
        ),
        Action::Add {
            file,
            mailbox,
            name,
            replace,
        } => {
            let data = std::fs::read(&file).map_err(|err| format!("{}: {err}", file.display()))?;
            let found = envelope::containers(&data).map_err(e)?;
            let origin = Origin::Local(format!(
                "added from {}",
                file.file_name().unwrap_or_default().to_string_lossy()
            ));
            for c in &found {
                let key = keys.and_then(|k| k.title(c.title()));
                store
                    .add_title(c.title(), name.as_deref(), key, false)
                    .map_err(e)?;
                let dropped = if replace && matches!(mailbox, BoxArg::Outbox) {
                    store
                        .replace_outbox(c, key, Some(origin.clone()))
                        .map_err(e)?
                } else {
                    store
                        .add_message(c, mailbox.into(), key, Some(origin.clone()))
                        .map_err(e)?
                };
                println!(
                    "{}",
                    json!({
                        "title": format!("{:08X}", c.title()),
                        "id": format!("{:016x}", c.message_id()),
                        "removed": dropped,
                    })
                );
            }
        }
        Action::Remove {
            title,
            message,
            mailbox,
        } => {
            let removed = match message {
                Some(id) => {
                    usize::from(store.remove_message(title, mailbox.into(), id).map_err(e)?)
                }
                None if store.titles().contains(&title) => store.remove_title(title).map_err(e)?,
                None => return Err(format!("title {title:08X} is not in the store")),
            };
            eprintln!("{removed} message(s) removed");
        }
        Action::Receive { title, state } => {
            let on = matches!(state, Switch::On);
            if on {
                let key = keys.and_then(|k| k.title(title));
                store.add_title(title, None, key, false).map_err(e)?;
            }
            store.set_receive(title, on).map_err(e)?;
        }
        Action::Identity { new } => {
            if new {
                let seed = ctrpass::identity::random_seed().map_err(|err| err.to_string())?;
                store.set_seed(&seed).map_err(e)?;
            }
            let id = store.identity().map_err(e)?;
            println!(
                "{}",
                json!({
                    "mac": mac_text(&id.mac),
                    "creator_mac": mac_text(&id.creator_mac),
                    "mii_id": hex(&id.mii_id.to_be_bytes()),
                })
            );
        }
    }
    Ok(())
}
