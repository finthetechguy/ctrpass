# ctrpass

Library, Linux command-line tool and desktop app for StreetPass-ing nearby Nintendo 3DS consoles over local wireless.

<!-- TODO screenshot: StreetPass page with a nearby console -->
<!-- TODO screenshot: Messages page and a Mii Plaza message -->

## Features

- Find nearby consoles with StreetPass enabled and exchange messages with them.
- Keep an inbox and outbox, and choose which games to receive messages for.
- Create and inspect StreetPass Mii Plaza messages.
- Import messages from a CEC save image, folder or message file.
- Copy inbox messages to an existing StreetPass folder in Azahar, AzaharPlus, Citra or Lime3DS.
- Decode StreetPass packet captures and inspect message files as JSON.

## Requirements

- Rust 1.85 or newer to build from source.
- Linux, `iw` and `ip` (iproute2) for local wireless.
- A Wi-Fi adapter that supports monitor mode for scanning and frame injection for exchanges. ctrpass uses active monitor mode when available or instead tries mixed mode on channel 1.
- For exchanges, an `aes_keys.txt` dumped from your own 3DS. It needs `slot0x2EKeyN` or both `slot0x2EKeyX` and `slot0x2EKeyY`, plus `streetpassHmacKey`. Reading or creating Mii records also needs the key for slot `0x31`. Creating signed messages requires a signing key for the game.

For the desktop app:

- Python 3.10 or newer.
- GTK 4, libadwaita 1.7 or newer, and PyGObject 3.48 or newer.
- polkit.

## Building

```sh
cargo build --release
```

To build the Python module for development, run this from the repository root in an active Python virtual environment with maturin installed.

```sh
maturin develop --release --manifest-path crates/ctrpass-py/Cargo.toml
```

## Installing the desktop app

The app runs as your user and starts `ctrpass serve` through polkit when it needs the radio.

```sh
sudo sh app/install.sh --no-rule
python3 -m pip install ./crates/ctrpass-py ./app
```

Open `ctrpass-app` and select your key file and wireless adapter in **Preferences**. The installer option shown above keeps the polkit password prompt. To allow your account to start the helper without a prompt, use `--user "$(id -un)"` in place of `--no-rule`, then log out and back in.

## Preparing a key file

Start with your own console's `aes_keys.txt`. Use `ctrpass keys` to add the StreetPass HMAC key from your own decrypted StreetPass system module (`code.bin`) and the game signing keys from a CEC save image or folder.

```sh
ctrpass --keys aes_keys.txt keys --module code.bin --titles cec-save.bin
```

## Command line

Radio commands need root or `CAP_NET_ADMIN` and `CAP_NET_RAW`. The key file comes from `--keys` or `CTRPASS_KEYS`. Replace `wlan1` with your wireless interface. The exchange example uses a separate store for commands run with `sudo`.

```sh
# List nearby consoles with StreetPass enabled
sudo ctrpass scan -i wlan1

# Enable receiving Mii Plaza messages in the CLI store
sudo ctrpass --store /var/lib/ctrpass store receive 00020800 on

# Exchange messages with nearby consoles until interrupted
sudo ctrpass --store /var/lib/ctrpass --keys aes_keys.txt exchange -i wlan1

# Inspect a StreetPass message as JSON
ctrpass info message.bin

# Preview copying the CLI inbox to an emulator's StreetPass folder
sudo ctrpass --store /var/lib/ctrpass export --dry-run /path/to/emulator/data
```

`ctrpass --help` lists the commands. Run `ctrpass <command> --help` for its options.

## License

[MIT](LICENSE).
