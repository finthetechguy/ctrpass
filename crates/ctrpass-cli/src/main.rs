use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use clap::{Parser, Subcommand};
use ctrpass::keys::Keys;

mod decode;
mod exchange;
mod info;
mod store;

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
    /// Manage the messages, titles and identity in the store.
    Store {
        #[command(subcommand)]
        action: store::Action,
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
        Command::Store { action } => {
            let keys = optional_keys(&cli.keys)?;
            let path = cli.store.map_or_else(store::default_path, Ok)?;
            store::run(&path, action, keys.as_ref())
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
