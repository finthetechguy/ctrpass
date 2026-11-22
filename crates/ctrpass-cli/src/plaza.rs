use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use clap::{Args, Subcommand};
use ctrpass::cec::Container;
use ctrpass::cec::container::{EXT_ICON, EXT_INFO_TEXT, ICON_LEN, Timestamp};
use ctrpass::cec::icon::Icon;
use ctrpass::keys::{Key, Keys};
use ctrpass::mii::{RECORD_SIZE, Record, STORED_SIZE, WRAPPED_SIZE};
use ctrpass::plaza::{self, Names, Plaza};
use serde_json::json;

use crate::{hex, mac_text};

/// Text shown with a new message when no other is given.
const STANDARD_TEXT: &str = "A visitor arrived";
const STANDARD_NAME: &str = "ctrpass";

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
    #[arg(long, value_parser = parse_u64)]
    featured: Option<u64>,
    /// Featured software's short name, in every language.
    #[arg(long)]
    short: Option<String>,
    #[arg(long)]
    long: Option<String>,
    #[arg(long)]
    publisher: Option<String>,
    /// PNG for the featured software's icons, scaled to 24x24 and 48x48.
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

fn parse_u64(s: &str) -> Result<u64, String> {
    u64::from_str_radix(s.trim_start_matches("0x"), 16).map_err(|e| e.to_string())
}

fn read(path: &Path) -> Result<Vec<u8>, String> {
    std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))
}

/// The Plaza body and message, if any, a file holds.
fn load(path: &Path) -> Result<(Plaza, Option<Container>), String> {
    let bytes = read(path)?;
    match Container::parse(&bytes) {
        Ok(c) => {
            if c.title() != plaza::TITLE {
                return Err(format!(
                    "{} is a message for {:08X}, not the Plaza",
                    path.display(),
                    c.title()
                ));
            }
            Ok((Plaza::parse(c.body()).map_err(|e| e.to_string())?, Some(c)))
        }
        Err(_) => Ok((Plaza::parse(&bytes).map_err(|e| e.to_string())?, None)),
    }
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

fn read_png(path: &Path) -> Result<(usize, usize, Vec<[u8; 3]>), String> {
    let e = |e: png::DecodingError| format!("{}: {e}", path.display());
    let mut decoder = png::Decoder::new(std::io::Cursor::new(read(path)?));
    decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = decoder.read_info().map_err(e)?;
    let mut buf = vec![0; reader.output_buffer_size().ok_or("image too large")?];
    let info = reader.next_frame(&mut buf).map_err(e)?;
    let channels = info.color_type.samples();
    let pixels = buf[..info.buffer_size()]
        .chunks(channels)
        .map(|p| match channels {
            1 | 2 => [p[0]; 3],
            _ => [p[0], p[1], p[2]],
        })
        .collect();
    Ok((info.width as usize, info.height as usize, pixels))
}

fn icon_from_png(path: &Path, size: usize) -> Result<Icon, String> {
    let (w, h, rgb) = read_png(path)?;
    Ok(Icon::from_rgb(size, w, h, &rgb))
}

fn write_png(path: &Path, icon: &Icon) -> Result<(), String> {
    let e = |e: png::EncodingError| format!("{}: {e}", path.display());
    let file = std::fs::File::create(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut enc = png::Encoder::new(
        std::io::BufWriter::new(file),
        icon.size as u32,
        icon.size as u32,
    );
    enc.set_color(png::ColorType::Rgb);
    enc.set_depth(png::BitDepth::Eight);
    let mut w = enc.write_header().map_err(e)?;
    w.write_image_data(&icon.to_rgb().concat()).map_err(e)
}

fn random_id() -> u64 {
    let mut b = [0u8; 8];
    getrandom::fill(&mut b).expect("the system random source failed");
    u64::from_le_bytes(b)
}

pub fn build(args: &BuildArgs, keys: &Keys, store: Option<&Path>) -> Result<(), String> {
    let record_key = keys
        .record
        .as_ref()
        .ok_or("the key file has no slot0x31 key, which Miis need")?;
    let title_key = keys
        .title(plaza::TITLE)
        .ok_or("the key file has no titleKey00020800")?;
    let e = |e: ctrpass::Error| e.to_string();

    let (mut p, template) = match &args.from {
        Some(path) => load(path)?,
        None => {
            let path = store.ok_or("no store to take an identity from: pass --from or --store")?;
            let id = ctrpass::store::Store::open(path)
                .and_then(|s| s.identity())
                .map_err(e)?;
            let mii = Record::new(
                STANDARD_NAME,
                id.mii_id,
                id.creator_mac,
                id.face_shape,
                id.gender,
            )
            .map_err(e)?;
            let mut p = Plaza::new(record_key, &mii).map_err(e)?;
            p.set_profile(&plaza::Profile {
                hat: id.hat,
                dream: id.dream,
                hobby: id.hobby,
                ..p.profile()
            });
            p.set_names(&Names {
                short: STANDARD_NAME.into(),
                long: STANDARD_NAME.into(),
                publisher: STANDARD_NAME.into(),
            })
            .map_err(e)?;
            (p, None)
        }
    };

    if args.mii.is_some() || args.name.is_some() || args.creator.is_some() {
        let mut mii = match &args.mii {
            Some(path) => load_mii(path, record_key)?,
            None => p.mii(record_key).map_err(e)?,
        };
        if let Some(n) = &args.name {
            mii.set_name(n).map_err(e)?;
        }
        if let Some(n) = &args.creator {
            mii.set_creator_name(n).map_err(e)?;
        }
        p.set_mii(record_key, &mii).map_err(e)?;
    }
    if let Some(g) = &args.greeting {
        p.set_greeting(g).map_err(e)?;
    }
    if let Some(id) = args.featured {
        p.set_featured_id(id);
    }
    if args.short.is_some() || args.long.is_some() || args.publisher.is_some() {
        let old = p.names(1);
        p.set_names(&Names {
            short: args.short.clone().unwrap_or(old.short),
            long: args.long.clone().unwrap_or(old.long),
            publisher: args.publisher.clone().unwrap_or(old.publisher),
        })
        .map_err(e)?;
    }
    if let Some(path) = &args.icon {
        p.set_icons(&icon_from_png(path, 24)?, &icon_from_png(path, 48)?)
            .map_err(e)?;
    }
    if args.country.is_some() || args.region.is_some() {
        let (c, r) = p.place(1);
        p.set_place(
            args.country.as_deref().unwrap_or(&c),
            args.region.as_deref().unwrap_or(&r),
        )
        .map_err(e)?;
    }
    let mut prof = p.profile();
    prof.hits = args.hits.unwrap_or(prof.hits);
    prof.population = args.population.unwrap_or(prof.population);
    prof.hat = args.hat.unwrap_or(prof.hat);
    prof.pet = args.pet.unwrap_or(prof.pet);
    prof.dream = args.dream.unwrap_or(prof.dream);
    prof.hobby = args.hobby.unwrap_or(prof.hobby);
    p.set_profile(&prof);
    if let Some(b) = args.bubble {
        p.set_bubble(b);
    }
    let problems = p.problems();
    if !problems.is_empty() {
        return Err(format!(
            "the Plaza would not keep this message: {}",
            problems.join("; ")
        ));
    }

    let icon = match (
        &args.message_icon,
        template.as_ref().and_then(|c| c.ext_header(EXT_ICON)),
    ) {
        (Some(path), _) => icon_from_png(path, 40)?,
        (None, Some(raw)) if raw.len() == ICON_LEN => Icon::decode(40, raw).map_err(e)?,
        _ => Icon::standard(40),
    };
    let text = match (
        &args.text,
        template.as_ref().and_then(|c| c.ext_header(EXT_INFO_TEXT)),
    ) {
        (Some(t), _) => t.clone(),
        (None, Some(raw)) => utf16z(raw),
        _ => STANDARD_TEXT.into(),
    };
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let created = Timestamp::from_unix(now.as_secs(), now.subsec_millis() as u16);
    let msg = p
        .message(title_key, random_id(), created, &icon, &text)
        .map_err(e)?;
    std::fs::write(&args.out, msg.as_bytes())
        .map_err(|err| format!("{}: {err}", args.out.display()))?;
    println!(
        "{}",
        json!({"out": args.out.display().to_string(), "id": format!("{:016x}", msg.message_id()), "size": msg.len()})
    );
    Ok(())
}

fn utf16z(raw: &[u8]) -> String {
    let units: Vec<u16> = raw
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .take_while(|&u| u != 0)
        .collect();
    String::from_utf16_lossy(&units)
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
        write_png(path, &p.icons().1)?;
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
            "text": c.ext_header(EXT_INFO_TEXT).map(utf16z),
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
