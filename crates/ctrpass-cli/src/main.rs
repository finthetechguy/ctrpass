use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use ctrpass::keys::Keys;

mod decode;

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
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Print the StreetPass frames in a capture as JSON, decrypting links when keys are available.
    Decode { pcap: PathBuf },
}

fn key_path(path: &Option<PathBuf>) -> Option<PathBuf> {
    path.clone()
        .or_else(|| std::env::var_os("CTRPASS_KEYS").map(PathBuf::from))
}

fn load_keys(path: &Option<PathBuf>) -> Result<Keys, String> {
    let path = key_path(path).ok_or("no key file: pass --keys or set CTRPASS_KEYS")?;
    Keys::load(&path).map_err(|e| e.to_string())
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

fn run(cli: Cli) -> Result<(), String> {
    match cli.command {
        Command::Decode { pcap } => {
            let keys = match key_path(&cli.keys) {
                Some(_) => Some(load_keys(&cli.keys)?),
                None => None,
            };
            decode::run(&pcap, keys.as_ref())
        }
    }
}

fn main() -> ExitCode {
    match run(Cli::parse()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("ctrpass: {e}");
            ExitCode::FAILURE
        }
    }
}
