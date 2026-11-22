use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use clap::{Parser, Subcommand};
use ctrpass::keys::Keys;

mod decode;
mod exchange;
mod info;
mod plaza;
#[cfg(target_os = "linux")]
mod serve;
mod store;
mod title;
mod transfer;

#[derive(Parser)]
#[command(
    name = "ctrpass",
    version,
    about = "StreetPass with Nintendo 3DS consoles"
)]
struct Cli {
    /// Key file; defaults to $CTRPASS_KEYS.
    #[arg(long, global = true)]
    keys: Option<PathBuf>,
    /// Store folder; defaults to $CTRPASS_STORE, then the per-user data folder.
    #[arg(long, global = true)]
    store: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Print the StreetPass frames in a capture as JSON, decrypting links when keys are available.
    Decode { pcap: PathBuf },
    /// Print a CEC message, envelope or exchange object as JSON, checking signatures when keys are available.
    Info { file: PathBuf },
    /// List nearby consoles from their StreetPass probes.
    Scan {
        /// Wireless interface to take over while scanning.
        #[arg(short, long)]
        interface: String,
        /// Channels to listen on, in order.
        #[arg(long, value_delimiter = ',', default_values_t = ctrpass::scan::CHANNELS)]
        channels: Vec<u8>,
        /// Seconds to listen on each channel.
        #[arg(long, default_value_t = 2.0)]
        dwell: f64,
        /// Passes over all channels.
        #[arg(long, default_value_t = 3)]
        rounds: u32,
        /// Take over the interface even if it is connected to a network.
        #[arg(long)]
        force: bool,
    },
    /// Exchange the store's messages with nearby consoles until stopped.
    Exchange {
        /// Wireless interface to take over.
        #[arg(short, long)]
        interface: String,
        #[arg(long, value_enum, default_value_t = exchange::RoleArg::Lead)]
        role: exchange::RoleArg,
        /// Channels to look for consoles on, in order.
        #[arg(long, value_delimiter = ',', default_values_t = ctrpass::scan::CHANNELS)]
        channels: Vec<u8>,
        /// Seconds each pass waits for a console.
        #[arg(long, default_value_t = 60.0)]
        window: f64,
        /// Stop after this many passes instead of running until interrupted.
        #[arg(long)]
        passes: Option<u32>,
        /// Take over the interface even if it is connected to a network.
        #[arg(long)]
        force: bool,
        #[arg(long, value_enum, default_value_t = RadioMode::Auto)]
        radio_mode: RadioMode,
    },
    /// Build and read Mii Plaza messages.
    Plaza {
        #[command(subcommand)]
        action: plaza::Action,
    },
    /// Build and read messages for any title ctrpass knows the fields of.
    Title {
        #[command(subcommand)]
        action: title::Action,
    },
    /// Manage the messages, titles and identity in the store.
    Store {
        #[command(subcommand)]
        action: store::Action,
    },
    /// Copy messages and title keys into the store from a save image, CEC folder or message file.
    Import {
        from: PathBuf,
        /// File every message in this box instead of the one it was kept in.
        #[arg(long = "to", value_enum)]
        to: Option<store::BoxArg>,
        /// Only list what would be imported.
        #[arg(long)]
        dry_run: bool,
    },
    /// Deliver the store's messages into a console-shaped CEC folder, such as an emulator's.
    Export {
        /// CEC folder or emulator data folder; found automatically when one emulator is installed.
        dest: Option<PathBuf>,
        #[arg(long = "box", value_enum, default_value_t = store::BoxArg::Inbox)]
        mailbox: store::BoxArg,
        /// Only this title, in hex.
        #[arg(long, value_parser = store::parse_hex::<u32>)]
        title: Option<u32>,
        /// Write plain message files as TITLE/_id instead.
        #[arg(long)]
        raw: bool,
        /// Report what would be delivered without writing.
        #[arg(long)]
        dry_run: bool,
        /// Write even while an emulator is running.
        #[arg(long)]
        force: bool,
    },
    /// Run as the desktop app's radio helper, taking JSON-line commands on stdin.
    Serve {
        #[arg(short, long)]
        interface: String,
        /// Take the interface even when another program manages it.
        #[arg(long)]
        force: bool,
    },
    /// Fill the key file from dumps of your own console.
    Keys {
        /// Decrypted StreetPass system module (code.bin), for the link HMAC key.
        #[arg(long)]
        module: Option<PathBuf>,
        /// File offset of the key in the module, when the search does not find it.
        #[arg(long, value_parser = store::parse_hex::<usize>)]
        offset: Option<usize>,
        /// CEC save image or folder, for each title's signing key; repeatable.
        #[arg(long = "titles")]
        titles: Vec<PathBuf>,
    },
}

static STOP: AtomicBool = AtomicBool::new(false);

fn key_path(path: &Option<PathBuf>) -> Option<PathBuf> {
    path.clone()
        .or_else(|| std::env::var_os("CTRPASS_KEYS").map(PathBuf::from))
}

fn load_keys(path: &Option<PathBuf>) -> Result<Keys, String> {
    let path = key_path(path).ok_or("no key file: pass --keys or set CTRPASS_KEYS")?;
    Keys::load(&path).map_err(|e| e.to_string())
}

/// Loads keys only when a key file was named.
fn optional_keys(path: &Option<PathBuf>) -> Result<Option<Keys>, String> {
    key_path(path).map(|_| load_keys(path)).transpose()
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

pub(crate) fn mac_text(m: &[u8]) -> String {
    m.iter()
        .map(|b| format!("{b:02x}"))
        .collect::<Vec<_>>()
        .join(":")
}

fn found_json(f: &ctrpass::scan::Found) -> serde_json::Value {
    serde_json::json!({
        "mac": mac_text(&f.mac),
        "token": f.token.map(|t| hex(&t)),
        "channel": f.channel,
        "signal": f.signal,
        "services": f.services.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
    })
}

/// How the card is set up for exchanging.
#[derive(Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
enum RadioMode {
    /// Active monitor when the card supports it, mixed mode otherwise.
    Auto,
    Active,
    /// A plain monitor and a station interface, on channel 1 only.
    Mixed,
}

#[cfg(target_os = "linux")]
fn open_radio(
    interface: &str,
    mac: Option<ctrpass::wifi::frame::Mac>,
    force: bool,
    mode: Option<ctrpass::radio::linux::Mode>,
) -> Result<ctrpass::radio::linux::LinuxRadio, String> {
    let radio =
        ctrpass::radio::linux::LinuxRadio::open(interface, mac, force, mode).map_err(|e| {
            if e.kind() == std::io::ErrorKind::PermissionDenied
                || e.to_string().contains("Operation not permitted")
            {
                format!("{e} (needs root or CAP_NET_ADMIN and CAP_NET_RAW)")
            } else {
                e.to_string()
            }
        })?;
    Ok(radio)
}

fn run(cli: Cli) -> Result<(), String> {
    match cli.command {
        Command::Decode { pcap } => {
            let keys = optional_keys(&cli.keys)?;
            decode::run(&pcap, keys.as_ref())
        }
        Command::Info { file } => {
            let keys = optional_keys(&cli.keys)?;
            info::run(&file, keys.as_ref())
        }
        #[cfg(target_os = "linux")]
        Command::Scan {
            interface,
            channels,
            dwell,
            rounds,
            force,
        } => {
            let mut radio = open_radio(
                &interface,
                None,
                force,
                Some(ctrpass::radio::linux::Mode::Listen),
            )?;
            let found = ctrpass::scan::scan(
                &mut radio,
                &channels,
                Duration::from_secs_f64(dwell),
                rounds,
                &STOP,
                |f| println!("{}", found_json(f)),
            )
            .map_err(|e| e.to_string())?;
            eprintln!("{} console(s) heard", found.len());
            Ok(())
        }
        #[cfg(not(target_os = "linux"))]
        Command::Scan { .. } => Err("scanning needs Linux".into()),
        #[cfg(target_os = "linux")]
        Command::Exchange {
            interface,
            role,
            channels,
            window,
            passes,
            force,
            radio_mode,
        } => {
            use ctrpass::radio::linux::Mode;
            let keys = load_keys(&cli.keys)?;
            let path = cli.store.map_or_else(store::default_path, Ok)?;
            let mut store = ctrpass::store::Store::open(&path).map_err(|e| e.to_string())?;
            let mac = store.identity().map_err(|e| e.to_string())?.mac;
            let mode = match radio_mode {
                RadioMode::Auto => None,
                RadioMode::Active => Some(Mode::Active),
                RadioMode::Mixed => Some(Mode::Mixed),
            };
            let mut radio = open_radio(&interface, Some(mac), force, mode)?;
            let mut channels = channels;
            if let Some(fixed) = radio.fixed_channel() {
                eprintln!("ctrpass: {interface} is in mixed mode, so only channel {fixed} is used");
                channels = vec![fixed];
            }
            let opts = exchange::Options {
                role,
                channels,
                window: Duration::from_secs_f64(window),
                passes,
            };
            exchange::run(&mut radio, &mut store, &keys, &opts, &STOP)
        }
        #[cfg(not(target_os = "linux"))]
        Command::Exchange { .. } => Err("exchanging needs Linux".into()),
        Command::Plaza { action } => match action {
            plaza::Action::Build(args) => {
                let keys = load_keys(&cli.keys)?;
                let store = match &args.from {
                    Some(_) => None,
                    None => Some(cli.store.map_or_else(store::default_path, Ok)?),
                };
                plaza::build(&args, &keys, store.as_deref())
            }
            plaza::Action::Show {
                file,
                icon_out,
                mii_out,
            } => {
                let keys = optional_keys(&cli.keys)?;
                plaza::show(
                    &file,
                    keys.as_ref(),
                    icon_out.as_deref(),
                    mii_out.as_deref(),
                )
            }
        },
        Command::Title { action } => match action {
            title::Action::List => {
                title::list();
                Ok(())
            }
            title::Action::Show { file, images } => {
                let keys = optional_keys(&cli.keys)?;
                title::show(&file, keys.as_ref(), images.as_deref())
            }
            title::Action::Build(args) => {
                let keys = load_keys(&cli.keys)?;
                let t = ctrpass::title::find(&args.title)
                    .ok_or_else(|| format!("no title {:?}; see title list", args.title))?;
                let store = match &args.common.from {
                    Some(_) => None,
                    None => Some(cli.store.map_or_else(store::default_path, Ok)?),
                };
                title::build(t, &args.common, &keys, store.as_deref())
            }
        },
        Command::Store { action } => {
            let keys = optional_keys(&cli.keys)?;
            let path = cli.store.map_or_else(store::default_path, Ok)?;
            store::run(&path, action, keys.as_ref())
        }
        Command::Import { from, to, dry_run } => {
            let path = cli.store.map_or_else(store::default_path, Ok)?;
            transfer::import(&path, &from, to, dry_run)
        }
        Command::Export {
            dest,
            mailbox,
            title,
            raw,
            dry_run,
            force,
        } => {
            let path = cli.store.map_or_else(store::default_path, Ok)?;
            let args = transfer::ExportArgs {
                dest: dest.as_deref(),
                mailbox: mailbox.into(),
                title,
                raw,
                dry_run,
                force,
            };
            transfer::export(&path, &args)
        }
        #[cfg(target_os = "linux")]
        Command::Serve { interface, force } => serve::run(&interface, force),
        #[cfg(not(target_os = "linux"))]
        Command::Serve { .. } => Err("the helper needs Linux".into()),
        Command::Keys {
            module,
            offset,
            titles,
        } => {
            let out = key_path(&cli.keys).map_or_else(transfer::default_key_path, Ok)?;
            transfer::extract_keys(&out, module.as_deref(), offset, &titles)
        }
    }
}

fn main() -> ExitCode {
    let _ = ctrlc::set_handler(|| STOP.store(true, Ordering::Relaxed));
    match run(Cli::parse()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("ctrpass: {e}");
            ExitCode::FAILURE
        }
    }
}
