use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use ctrpass::keys::Keys;

mod decode;
mod info;

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
    /// Print a CEC message, envelope or exchange object as JSON, checking signatures when keys are available.
    Info { file: PathBuf },
}

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
