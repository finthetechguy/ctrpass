# Title plugins

A plugin tells ctrpass how a game's StreetPass message body is laid out: its fields, the values each
field allows, and how to make a new body. With a plugin, the game shows up in `ctrpass title` and
in the app's message editor, without rebuilding ctrpass.

ctrpass keeps every key. A plugin only reads and writes the body; ctrpass signs the message, and it
protects any Mii record in the body with the slot0x31 key itself.

## Where plugins go

Put plugin files in `$XDG_DATA_HOME/ctrpass/titles/` (usually `~/.local/share/ctrpass/titles/`),
or in the folder named by `$CTRPASS_PLUGINS`. Two kinds of file load from there:

- **Layout files** (`.toml`): every field sits at a fixed spot in a body of fixed size. No code runs.
- **Modules** (`.wasm`): WebAssembly code reads and writes the body, for formats a layout can't
  describe, such as lists or compressed data.

Mii Plaza ships with ctrpass as a built-in layout file, [`mii-plaza.toml`](../crates/ctrpass/titles/mii-plaza.toml),
which also serves as a full example. A plugin file for the same title replaces a built-in one,
with a warning. Two plugin files for the same title are an error, and the second one is skipped.

A plugin that fails to load is listed with the reason, and other titles keep working:

```sh
ctrpass plugins list           # loaded plugins, errors and warnings, as JSON
ctrpass plugins check FILE     # load a file and make, read and rewrite a body with it
```

`plugins check` fails when the values read from a new body don't write back to the same bytes,
or when a new body has problems.

## The manifest

Both kinds of plugin start with the same keys:

| Key             | Meaning                                                        |
|-----------------|----------------------------------------------------------------|
| `api`           | Plugin interface version. Must be `1`.                         |
| `title`         | Title ID in hex, such as `"00020800"`.                         |
| `name`          | Game name, shown in the app and accepted by `ctrpass title`.   |
| `version`       | The plugin's own version.                                      |
| `author`        | Who wrote the plugin.                                          |
| `message_text`  | Text shown with a new message.                                 |
| `send_mode`     | CEC send mode of new messages. Default `0`.                    |
| `message_flags` | Flags of new messages. Default `3`.                            |

## Fields

Each field is a `[[field]]` table in a layout file, or an entry in the `fields` array of a module's
manifest.

| Key        | Meaning                                                        |
|------------|----------------------------------------------------------------|
| `key`      | Name used in JSON values and with `--set key=value`.           |
| `label`    | Name shown in forms.                                           |
| `type`     | One of the types below.                                        |
| `help`     | Short text shown with the field. Optional.                     |
| `advanced` | `true` to tuck the field under "More" in forms.                |

| Type          | Value                         | Keys                                             |
|---------------|-------------------------------|--------------------------------------------------|
| `text`        | UTF-16 text                   | `max` characters, `forbid` characters            |
| `number`      | Whole number                  | `min`, `max`, `exclude` list, `hex` for display  |
| `bytes`       | Hex string of `len` bytes     | `len`                                            |
| `image`       | Square RGB565 tiled image     | `size` in pixels, a multiple of 8                |
| `mii`         | 96-byte Mii record            | `at`: where the protected record sits            |
| `mii_name`    | The Mii's name                | `mii`: key of the Mii field, if there are several |
| `mii_creator` | The Mii's creator name        | `mii`                                            |
| `mii_id`      | The Mii's ID                  | `mii`                                            |
| `mii_mac`     | The MAC the Mii was made on   | `mii`                                            |

Numbers can be written as integers or as text, such as `max = "0xfffffffffffffffe"` for values
too large for a TOML integer.

ctrpass handles every `mii` field itself in both kinds of plugin: it decrypts the record, applies
the `mii_*` fields, refuses a Mii the console would drop, and protects it again. A new body gets a
Mii made from the store's identity. Plugins never see the key or the protected bytes.

ctrpass checks every value against these rules before a plugin writes it. Values the plugin
reads back that break the rules are listed as problems, and the app and CLI refuse to sign a
message with problems.

## Layout files

A layout file adds the body size and where each field sits:

```toml
api = 1
title = "00ABCD01"
name = "High Scores"
version = "1.0.0"
author = "Someone"
message_text = "New scores"
size = 0x200

[[field]]
key = "player"
label = "Player"
type = "text"
at = 0x10
bytes = 0x20          # room in the body; max defaults to bytes / 2 - 1

[[field]]
key = "score"
label = "Score"
type = "number"
at = 0x04
width = 4             # 1, 2, 4 or 8 bytes
big_endian = false    # the default
max = 999999

[[computed]]
type = "crc32"
at = 0x00
start = 0x04          # covers 0x04 to the end of the body
```

Placement keys:

| Key          | Meaning                                                                |
|--------------|------------------------------------------------------------------------|
| `at`         | Offset of the field in the body.                                       |
| `bytes`      | Text: bytes of room, zero padded.                                      |
| `width`      | Number: size in bytes. `big_endian` sets the byte order.              |
| `copies`     | Write the value this many times, `stride` bytes apart. Default 1.     |
| `stride`     | Distance between copies. Defaults to the field's own size.            |
| `read_copy`  | Which copy to read, counting from 0. Default 0.                       |
| `clear`      | `[[at, len], ...]` ranges zeroed whenever the field is written.        |
| `scale_from` | Image: when only the named image field is set, scale it into this one. |

`copies` suits games that keep the same text once per language.

### New bodies

```toml
[[fill]]                      # bytes a new body starts with; the rest are zero
at = 0x30
hex = "4e 4f 54 45"

[create]
values = { score = 0 }       # image fields also take "standard", ctrpass's default icon
identity = { score = "hat" } # field key = identity value
```

The identity values are `mii_id`, `creator_mac`, `face_shape`, `gender`, `hat`, `dream` and
`hobby`, all made from the store's seed.

### Checks and computed values

```toml
[[check]]                     # a problem unless the bytes at `at` match one of these
at = 0x30
one_of = ["4e4f5445"]
problem = "the body has no header"

[[computed]]                  # number of UTF-16 characters in a text field
type = "length"
at = 0x08
width = 1
field = "player"

[[computed]]                  # entries of `stride` bytes that are not all zero
type = "count"
at = 0x09
width = 1
start = 0x100
stride = 0x10
entries = 16
```

Computed values are written after every change, checksums last, so a checksum covers the other
computed values. A computed value that doesn't match the body is listed as a problem.

## Modules

A module is a WebAssembly file that imports nothing. It runs in an interpreter with no access to
files, the network or keys. Each call runs in a fresh instance limited to 64 MiB of memory and a
fixed number of instructions, so a plugin that hangs or crashes only fails its own call.

It exports `memory` and these functions. Inputs are passed as a pointer and length in the module's
memory:

| Export                                    | Input                       | Result                       |
|-------------------------------------------|-----------------------------|------------------------------|
| `ctrpass_alloc(len) -> ptr`               |                             | room for `len` bytes of input |
| `ctrpass_manifest() -> out`               |                             | manifest and `fields`, JSON  |
| `ctrpass_create(ptr, len) -> out`         | identity values, JSON       | new body                     |
| `ctrpass_read(ptr, len) -> out`           | body                        | field values, JSON object    |
| `ctrpass_write(ptr, len, ptr, len) -> out`| body, then values as JSON   | new body                     |
| `ctrpass_problems(ptr, len) -> out`       | body                        | JSON array of text           |

Pointers and lengths are `i32`. Each `out` is an `i64` with the result's pointer in the high 32
bits and its length in the low 32. The result's first byte is `0` when the rest is the result,
or `1` when the rest is an error message in UTF-8.

`ctrpass_read` returns every field except `mii` and `mii_*` fields, and `ctrpass_write` is only
given values for those same fields. Values are JSON: text as strings, numbers as integers, and
bytes and images as hex strings.

[`examples/note-board`](../examples/note-board) is a module written in Rust for a made-up
title with a list of notes and a checksum. Build it with:

```sh
rustup target add wasm32-unknown-unknown
cargo build --release --target wasm32-unknown-unknown --manifest-path examples/note-board/Cargo.toml
ctrpass plugins check examples/note-board/target/wasm32-unknown-unknown/release/note_board.wasm
```

Any language that builds WebAssembly without imports works the same way.
