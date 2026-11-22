use std::path::{Path, PathBuf};

use clap::{Args, Subcommand};
use ctrpass::cec::Container;
use ctrpass::keys::{Key, Keys};
use ctrpass::mii::{RECORD_SIZE, Record, STORED_SIZE, WRAPPED_SIZE};
use ctrpass::plaza::{self, Plaza, PlazaTitle};
use serde_json::json;

use crate::title;
use crate::{hex, mac_text};

#[derive(Subcommand)]
pub enum Action {
    /// Write a signed Mii Plaza message, new or edited from an existing one.
    Build(Box<BuildArgs>),
    /// Print a Mii Plaza message or body as JSON.
    Show {
        file: PathBuf,
        /// Also write the featured software's large icon as a PNG.
        #[arg(long)]
        icon_out: Option<PathBuf>,
        /// Also write the Mii as a 96-byte record.
        #[arg(long)]
        mii_out: Option<PathBuf>,
    },
}

#[derive(Args)]
pub struct BuildArgs {
    /// Where to write the message.
    #[arg(short, long)]
    out: PathBuf,
    /// A Plaza message or body to start from; without it the store identity's Mii is used.
    #[arg(long)]
    pub from: Option<PathBuf>,
    /// A Mii record (96 or 92 bytes, or wrapped), or a Plaza message to take the Mii from.
    #[arg(long)]
    mii: Option<PathBuf>,
    /// Name shown for the Mii.
    #[arg(long)]
    name: Option<String>,
    /// Creator name shown on the Mii.
    #[arg(long)]
    creator: Option<String>,
    #[arg(long)]
    greeting: Option<String>,
    /// Title ID of the featured software, in hex.
    #[arg(long)]
    featured: Option<String>,
    /// Featured software's short name, in every language.
    #[arg(long)]
    short: Option<String>,
    #[arg(long)]
    long: Option<String>,
    #[arg(long)]
    publisher: Option<String>,
    /// PNG for the featured software's icons, scaled to 48x48 and 24x24.
    #[arg(long)]
    icon: Option<PathBuf>,
    /// PNG for the message's own icon, scaled to 40x40.
    #[arg(long)]
    message_icon: Option<PathBuf>,
    /// Text shown with the message.
    #[arg(long)]
    text: Option<String>,
    #[arg(long)]
    country: Option<String>,
    #[arg(long)]
    region: Option<String>,
    #[arg(long)]
    hits: Option<u32>,
    #[arg(long)]
    population: Option<u16>,
    #[arg(long)]
    hat: Option<u8>,
    #[arg(long)]
    pet: Option<u8>,
    #[arg(long)]
    dream: Option<u8>,
    #[arg(long)]
    hobby: Option<u8>,
    /// Speech bubble style.
    #[arg(long)]
    bubble: Option<u8>,
}

fn read(path: &Path) -> Result<Vec<u8>, String> {
    std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))
}

/// The Plaza body and message, if any, a file holds.
fn load(path: &Path) -> Result<(Plaza, Option<Container>), String> {
    let (body, msg) = title::load(path, Some(&PlazaTitle))?;
    Ok((Plaza::parse(&body).map_err(|e| e.to_string())?, msg))
}

fn load_mii(path: &Path, key: &Key) -> Result<Record, String> {
    let bytes = read(path)?;
    let e = |e: ctrpass::Error| format!("{}: {e}", path.display());
    match bytes.len() {
        RECORD_SIZE => Record::from_bytes(&bytes).map_err(e),
        STORED_SIZE => Record::from_stored(&bytes).map_err(e),
        WRAPPED_SIZE => ctrpass::mii::unwrap(key, &bytes).map_err(e),
        _ => load(path)?.0.mii(key).map_err(e),
    }
}

/// Builds through the generic title path, with each flag set as its field.
pub fn build(args: &BuildArgs, keys: &Keys, store: Option<&Path>) -> Result<(), String> {
    let mut sets = Vec::new();
    let mut set = |k: &str, v: Option<String>| {
        if let Some(v) = v {
            sets.push(format!("{k}={v}"));
        }
    };
    if let Some(path) = &args.mii {
        let key = keys
            .record
            .as_ref()
            .ok_or("the key file has no slot0x31 key, which Miis need")?;
        set("mii", Some(hex(load_mii(path, key)?.as_bytes())));
    }
    set("name", args.name.clone());
    set("creator", args.creator.clone());
    set("greeting", args.greeting.clone());
    set("featured", args.featured.clone());
    set("short", args.short.clone());
    set("long", args.long.clone());
    set("publisher", args.publisher.clone());
    set("country", args.country.clone());
    set("region", args.region.clone());
    set("hits", args.hits.map(|n| n.to_string()));
    set("population", args.population.map(|n| n.to_string()));
    set("hat", args.hat.map(|n| n.to_string()));
    set("pet", args.pet.map(|n| n.to_string()));
    set("dream", args.dream.map(|n| n.to_string()));
    set("hobby", args.hobby.map(|n| n.to_string()));
    set("bubble", args.bubble.map(|n| n.to_string()));
    let common = title::Common {
        out: args.out.clone(),
        from: args.from.clone(),
        sets,
        images: args
            .icon
            .iter()
            .map(|p| format!("icon={}", p.display()))
            .collect(),
        values: None,
        message_icon: args.message_icon.clone(),
        text: args.text.clone(),
    };
    title::build(&PlazaTitle, &common, keys, store)
}

pub fn show(
    file: &Path,
    keys: Option<&Keys>,
    icon_out: Option<&Path>,
    mii_out: Option<&Path>,
) -> Result<(), String> {
    let (p, msg) = load(file)?;
    let mii = match keys.and_then(|k| k.record.as_ref()) {
        Some(key) => match p.mii(key) {
            Ok(m) => {
                if let Some(path) = mii_out {
                    std::fs::write(path, m.as_bytes())
                        .map_err(|e| format!("{}: {e}", path.display()))?;
                }
                let (month, day) = m.birthday();
                json!({
                    "name": m.name(),
                    "creator": m.creator_name(),
                    "mii_id": format!("{:08x}", m.mii_id()),
                    "creator_mac": mac_text(&m.creator_mac()),
                    "gender": m.field("gender"),
                    "birthday": (month != 0).then(|| format!("{day}/{month}")),
                    "favourite_colour": m.field("favourite_colour"),
                    "problems": m.validate().iter().map(|i| i.to_string()).collect::<Vec<_>>(),
                })
            }
            Err(e) => json!({"error": e.to_string()}),
        },
        None => json!({"error": "no slot0x31 key to read the Mii with"}),
    };
    if let Some(path) = icon_out {
        title::write_png(path, &p.icons().1)?;
    }
    let (country, region) = p.place(1);
    let names = p.names(1);
    let history: Vec<_> = p
        .history()
        .iter()
        .map(|m| {
            json!({
                "mii_id": format!("{:08x}", m.mii_id),
                "mac": mac_text(&m.mac),
                "name": m.name,
                "creator": m.creator,
            })
        })
        .collect();
    let out = json!({
        "message": msg.as_ref().map(|c| json!({
            "id": format!("{:016x}", c.message_id()),
            "text": ctrpass::title::message_text(c),
            "signature": keys.and_then(|k| k.title(plaza::TITLE)).map(|k| if c.verify(k) { "valid" } else { "invalid" }),
        })),
        "mii": mii,
        "greeting": p.greeting(),
        "featured": {
            "title_id": format!("{:016x}", p.featured_id()),
            "short": names.short,
            "long": names.long,
            "publisher": names.publisher,
            "icon_kind": p.icon_kind(),
            "region_lockout": format!("{:08x}", p.region_lockout()),
            "ratings": hex(p.ratings()),
        },
        "country": country,
        "region": region,
        "address": format!("{:08x}", p.address()),
        "presence": format!("{:x}", p.presence()),
        "profile": p.profile(),
        "bubble": p.bubble(),
        "cache_count": p.cache_count(),
        "history": history,
        "problems": p.problems(),
    });
    println!("{}", serde_json::to_string_pretty(&out).unwrap());
    Ok(())
}
