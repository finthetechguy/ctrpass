use std::path::{Path, PathBuf};

use clap::{Args, Subcommand};
use ctrpass::cec::Container;
use ctrpass::cec::icon::Icon;
use ctrpass::identity::Identity;
use ctrpass::keys::Keys;
use ctrpass::title::{self, Context, Kind, Source, Title, Values};
use serde_json::{Value, json};

#[derive(Subcommand)]
pub enum Action {
    /// List the titles ctrpass can build messages for, with their fields, as JSON.
    List,
    /// Print the field values of a message as JSON.
    Show {
        file: PathBuf,
        /// Write each image field into this folder as a PNG.
        #[arg(long)]
        images: Option<PathBuf>,
    },
    /// Write a signed message for a title, new or edited from an existing one.
    Build(BuildArgs),
}

#[derive(Args)]
pub struct BuildArgs {
    /// Title ID in hex, or the title's name.
    pub title: String,
    #[command(flatten)]
    pub common: Common,
}

#[derive(Args, Default)]
pub struct Common {
    /// Where to write the message.
    #[arg(short, long)]
    pub out: PathBuf,
    /// A message or bare body to start from; without it a new body is made from the store identity.
    #[arg(long)]
    pub from: Option<PathBuf>,
    /// Set a field, as key=value; repeatable.
    #[arg(long = "set", value_name = "KEY=VALUE")]
    pub sets: Vec<String>,
    /// Set a field from a file, as key=file: a PNG for images, a record or message for Miis,
    /// raw bytes otherwise; repeatable.
    #[arg(long = "file", alias = "image", value_name = "KEY=FILE")]
    pub files: Vec<String>,
    /// A JSON object of field values, like the "values" that show prints.
    #[arg(long)]
    pub values: Option<PathBuf>,
    /// PNG for the message's own icon, scaled to 40x40.
    #[arg(long)]
    pub message_icon: Option<PathBuf>,
    /// Text shown with the message.
    #[arg(long)]
    pub text: Option<String>,
}

fn read(path: &Path) -> Result<Vec<u8>, String> {
    std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))
}

pub fn read_png(path: &Path) -> Result<(usize, usize, Vec<[u8; 3]>), String> {
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

pub fn icon_from_png(path: &Path, size: usize) -> Result<Icon, String> {
    let (w, h, rgb) = read_png(path)?;
    Ok(Icon::from_rgb(size, w, h, &rgb))
}

pub fn write_png(path: &Path, icon: &Icon) -> Result<(), String> {
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

/// The body and message, if any, a file holds, for `title` when given.
pub fn load(path: &Path, title: Option<&Title>) -> Result<(Vec<u8>, Option<Container>), String> {
    let bytes = read(path)?;
    match Container::parse(&bytes) {
        Ok(c) => {
            if let Some(t) = title.filter(|t| t.id() != c.title()) {
                return Err(format!(
                    "{} is a message for {:08X}, not {}",
                    path.display(),
                    c.title(),
                    t.name()
                ));
            }
            Ok((c.body().to_vec(), Some(c)))
        }
        Err(_) => Ok((bytes, None)),
    }
}

fn context<'a>(keys: Option<&'a Keys>, identity: Option<&'a Identity>) -> Context<'a> {
    Context {
        record_key: keys.and_then(|k| k.record.as_ref()),
        identity,
    }
}

fn split(arg: &str) -> Result<(&str, &str), String> {
    arg.split_once('=')
        .ok_or_else(|| format!("{arg:?} is not key=value"))
}

/// A field's value from a file, as `--file` takes it.
fn file_value(t: &Title, key: &str, path: &Path, keys: &Keys) -> Result<Value, String> {
    let e = |e: ctrpass::Error| format!("{}: {e}", path.display());
    let f = title::field(t, key).map_err(|e| e.to_string())?;
    if let Kind::Image { size } = f.kind {
        return Ok(Value::from(title::hex(
            &icon_from_png(path, size)?.encode(),
        )));
    }
    let bytes = read(path)?;
    let record_key = keys.record.as_ref();
    if let (Kind::Mii { .. }, Ok(c)) = (&f.kind, Container::parse(&bytes)) {
        // Take the first Mii from a message of any title that holds one.
        let from = title::by_id(c.title()).ok_or_else(|| {
            format!(
                "{}: no fields are known for {:08X}",
                path.display(),
                c.title()
            )
        })?;
        let ctx = context(Some(keys), None);
        let values = from.read(c.body(), &ctx).map_err(e)?;
        let mii = from
            .fields()
            .iter()
            .find(|g| matches!(g.kind, Kind::Mii { .. }))
            .and_then(|g| values.get(&g.key))
            .ok_or_else(|| format!("{} holds no Mii", path.display()))?;
        return title::check(f, mii).map_err(e);
    }
    title::from_file(f, &bytes, record_key).map_err(e)
}

pub fn build(t: &Title, args: &Common, keys: &Keys, store: Option<&Path>) -> Result<(), String> {
    let e = |e: ctrpass::Error| e.to_string();
    let title_key = keys
        .title(t.id())
        .ok_or_else(|| format!("the key file has no titleKey{:08X}", t.id()))?;
    let identity = match (&args.from, store) {
        (None, Some(path)) => Some(
            ctrpass::store::Store::open(path)
                .and_then(|s| s.identity())
                .map_err(e)?,
        ),
        (None, None) => {
            return Err("no store to take an identity from: pass --from or --store".into());
        }
        _ => None,
    };
    let ctx = context(Some(keys), identity.as_ref());
    let (body, template) = match &args.from {
        Some(path) => load(path, Some(t))?,
        None => (t.create(&ctx).map_err(e)?, None),
    };

    let mut values = Values::new();
    if let Some(path) = &args.values {
        let text =
            std::fs::read_to_string(path).map_err(|err| format!("{}: {err}", path.display()))?;
        let given: Values =
            serde_json::from_str(&text).map_err(|err| format!("{}: {err}", path.display()))?;
        for (k, v) in given {
            let f = title::field(t, &k).map_err(e)?;
            // Images print as a PNG path or a size note; load the first and skip the second.
            if let (Kind::Image { size }, Some(s)) = (&f.kind, v.as_str()) {
                if title::unhex(s).is_none() {
                    if s.ends_with(".png") {
                        let icon =
                            icon_from_png(&path.parent().unwrap_or(Path::new(".")).join(s), *size)?;
                        values.insert(k, Value::from(title::hex(&icon.encode())));
                    }
                    continue;
                }
            }
            values.insert(k, v);
        }
    }
    for arg in &args.sets {
        let (k, v) = split(arg)?;
        values.insert(
            k.into(),
            title::from_text(title::field(t, k).map_err(e)?, v).map_err(e)?,
        );
    }
    for arg in &args.files {
        let (k, path) = split(arg)?;
        values.insert(k.into(), file_value(t, k, Path::new(path), keys)?);
    }
    let body = t.write(&body, &values, &ctx).map_err(e)?;
    let problems = t.problems(&body);
    if !problems.is_empty() {
        return Err(format!(
            "{} would not keep this message: {}",
            t.name(),
            problems.join("; ")
        ));
    }
    let icon = args
        .message_icon
        .as_deref()
        .map(|p| icon_from_png(p, 40))
        .transpose()?;
    let msg = title::message(
        t,
        &body,
        title_key,
        template.as_ref(),
        icon.as_ref(),
        args.text.as_deref(),
    )
    .map_err(e)?;
    std::fs::write(&args.out, msg.as_bytes())
        .map_err(|err| format!("{}: {err}", args.out.display()))?;
    println!(
        "{}",
        json!({
            "out": args.out.display().to_string(),
            "title": format!("{:08X}", t.id()),
            "id": format!("{:016x}", msg.message_id()),
            "size": msg.len(),
        })
    );
    Ok(())
}

/// A title's plugin details, as the list commands print them.
pub fn about(t: &Title) -> Value {
    json!({
        "title": format!("{:08X}", t.id()),
        "name": t.name(),
        "version": t.version(),
        "author": t.author(),
        "plugin": match t.source() {
            Source::BuiltIn => Value::from("built-in"),
            Source::File(p) => Value::from(p.display().to_string()),
        },
    })
}

pub fn list() {
    let titles: Vec<_> = title::all()
        .iter()
        .map(|t| {
            let mut v = about(t);
            v["send_mode"] = t.send_mode().into();
            v["fields"] = json!(t.fields());
            v
        })
        .collect();
    println!("{}", serde_json::to_string_pretty(&titles).unwrap());
}

pub fn show(file: &Path, keys: Option<&Keys>, images: Option<&Path>) -> Result<(), String> {
    let (body, msg) = load(file, None)?;
    let id = msg
        .as_ref()
        .map(Container::title)
        .ok_or("a bare body does not say its title; show the whole message")?;
    let t = title::by_id(id).ok_or_else(|| format!("no fields are known for title {id:08X}"))?;
    let mut values = t
        .read(&body, &context(keys, None))
        .map_err(|e| e.to_string())?;
    for f in t.fields() {
        let Kind::Image { size } = f.kind else {
            continue;
        };
        let Some(icon) = title::Get(&values).image(&f.key, size) else {
            continue;
        };
        let shown = match images {
            Some(dir) => {
                let path = dir.join(format!("{}.png", f.key));
                write_png(&path, &icon)?;
                Value::from(path.display().to_string())
            }
            None => Value::from(format!("{size}x{size} image")),
        };
        values.insert(f.key.clone(), shown);
    }
    let c = msg.as_ref().unwrap();
    let out = json!({
        "title": format!("{:08X}", t.id()),
        "name": t.name(),
        "id": format!("{:016x}", c.message_id()),
        "text": title::message_text(c),
        "signature": keys.and_then(|k| k.title(t.id())).map(|k| if c.verify(k) { "valid" } else { "invalid" }),
        "values": values,
        "problems": t.problems(&body),
    });
    println!("{}", serde_json::to_string_pretty(&out).unwrap());
    Ok(())
}
