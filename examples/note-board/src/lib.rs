//! An example ctrpass plugin for a made-up title whose body holds a list of notes.
//!
//! Body layout:
//!
//! | at   | size  | value                                          |
//! |------|-------|------------------------------------------------|
//! | 0x00 | 4     | "NOTE"                                         |
//! | 0x04 | 1     | colour, 0 to 7                                 |
//! | 0x05 | 1     | number of notes                                |
//! | 0x08 | 4     | CRC-32 of every byte from 0x0c on, little end  |
//! | 0x0c | 0x20  | author, UTF-16, zero padded                    |
//! | 0x2c | ...   | each note: a length in UTF-16 units, then text |

use serde_json::{Map, Value, json};

const MAGIC: &[u8; 4] = b"NOTE";
const AUTHOR: usize = 0x0c;
const AUTHOR_BYTES: usize = 0x20;
const NOTES: usize = AUTHOR + AUTHOR_BYTES;
const MAX_NOTE: usize = 255;
const MAX_NOTES: usize = 32;

fn manifest() -> Value {
    json!({
        "api": 1,
        "title": "00ABCD00",
        "name": "Note Board",
        "version": "1.0.0",
        "author": "ctrpass",
        "send_mode": 0,
        "message_text": "New notes",
        "fields": [
            {"key": "author", "label": "Author", "type": "text", "max": AUTHOR_BYTES / 2 - 1},
            {"key": "notes", "label": "Notes", "help": "One note per line", "type": "text", "max": 1000},
            {"key": "colour", "label": "Colour", "type": "number", "max": 7},
        ],
    })
}

struct Board {
    colour: u8,
    author: String,
    notes: Vec<String>,
}

fn utf16(s: &str) -> Vec<u8> {
    s.encode_utf16().flat_map(u16::to_le_bytes).collect()
}

fn text(b: &[u8]) -> String {
    let units: Vec<u16> = b
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .take_while(|&u| u != 0)
        .collect();
    String::from_utf16_lossy(&units)
}

fn crc32(data: &[u8]) -> u32 {
    let mut c = !0u32;
    for &b in data {
        c ^= u32::from(b);
        for _ in 0..8 {
            c = if c & 1 != 0 {
                (c >> 1) ^ 0xedb8_8320
            } else {
                c >> 1
            };
        }
    }
    !c
}

impl Board {
    fn parse(b: &[u8]) -> Result<Self, String> {
        if b.len() < NOTES || &b[..4] != MAGIC {
            return Err("not a note board body".into());
        }
        let mut notes = Vec::new();
        let mut at = NOTES;
        for _ in 0..b[5] {
            let len = 2 * usize::from(*b.get(at).ok_or("the notes end early")?);
            let note = b.get(at + 1..at + 1 + len).ok_or("the notes end early")?;
            notes.push(text(note));
            at += 1 + len;
        }
        Ok(Board {
            colour: b[4],
            author: text(&b[AUTHOR..NOTES]),
            notes,
        })
    }

    fn to_bytes(&self) -> Vec<u8> {
        let mut b = vec![0; NOTES];
        b[..4].copy_from_slice(MAGIC);
        b[4] = self.colour;
        b[5] = self.notes.len() as u8;
        let author = utf16(&self.author);
        b[AUTHOR..AUTHOR + author.len()].copy_from_slice(&author);
        for note in &self.notes {
            let units = utf16(note);
            b.push((units.len() / 2) as u8);
            b.extend(units);
        }
        let crc = crc32(&b[AUTHOR..]);
        b[8..12].copy_from_slice(&crc.to_le_bytes());
        b
    }

    fn values(&self) -> Map<String, Value> {
        let mut v = Map::new();
        v.insert("author".into(), self.author.clone().into());
        v.insert("notes".into(), self.notes.join("\n").into());
        v.insert("colour".into(), self.colour.into());
        v
    }

    fn apply(&mut self, values: &Map<String, Value>) -> Result<(), String> {
        if let Some(a) = values.get("author").and_then(Value::as_str) {
            self.author = a.into();
        }
        if let Some(c) = values.get("colour").and_then(Value::as_u64) {
            self.colour = c as u8;
        }
        if let Some(n) = values.get("notes").and_then(Value::as_str) {
            let notes: Vec<String> = n
                .lines()
                .filter(|l| !l.is_empty())
                .map(String::from)
                .collect();
            if notes.len() > MAX_NOTES {
                return Err(format!("at most {MAX_NOTES} notes"));
            }
            if notes.iter().any(|l| l.encode_utf16().count() > MAX_NOTE) {
                return Err(format!("a note is at most {MAX_NOTE} characters"));
            }
            self.notes = notes;
        }
        Ok(())
    }
}

fn problems(b: &[u8]) -> Vec<String> {
    let mut out = Vec::new();
    match Board::parse(b) {
        Ok(board) => {
            if board.colour > 7 {
                out.push(format!("colour {} is above 7", board.colour));
            }
            if b[8..12] != crc32(&b[AUTHOR..]).to_le_bytes() {
                out.push("the checksum does not match".into());
            }
        }
        Err(e) => out.push(e),
    }
    out
}

/// Input ctrpass wrote at a pointer it got from `ctrpass_alloc`.
fn input<'a>(ptr: i32, len: i32) -> &'a [u8] {
    // SAFETY: ctrpass writes exactly `len` bytes at a pointer this module allocated.
    unsafe { std::slice::from_raw_parts(ptr as *const u8, len as usize) }
}

fn json_input(ptr: i32, len: i32) -> Result<Map<String, Value>, String> {
    serde_json::from_slice(input(ptr, len)).map_err(|e| e.to_string())
}

/// Hands a result to ctrpass: status byte, then the result or the error text.
fn out(result: Result<Vec<u8>, String>) -> i64 {
    let mut buf = match result {
        Ok(b) => [vec![0], b].concat(),
        Err(e) => [vec![1], e.into_bytes()].concat(),
    };
    let (ptr, len) = (buf.as_mut_ptr() as u32 as i64, buf.len() as i64);
    // ctrpass reads the buffer, then drops the whole instance.
    std::mem::forget(buf);
    ptr << 32 | len
}

#[unsafe(no_mangle)]
pub extern "C" fn ctrpass_alloc(len: i32) -> i32 {
    let mut buf = Vec::<u8>::with_capacity(len as usize);
    let ptr = buf.as_mut_ptr();
    std::mem::forget(buf);
    ptr as i32
}

#[unsafe(no_mangle)]
pub extern "C" fn ctrpass_manifest() -> i64 {
    out(Ok(manifest().to_string().into_bytes()))
}

#[unsafe(no_mangle)]
pub extern "C" fn ctrpass_create(ptr: i32, len: i32) -> i64 {
    out(json_input(ptr, len).map(|identity| {
        let hat = identity.get("hat").and_then(Value::as_u64).unwrap_or(0);
        Board {
            colour: (hat % 8) as u8,
            author: String::new(),
            notes: Vec::new(),
        }
        .to_bytes()
    }))
}

#[unsafe(no_mangle)]
pub extern "C" fn ctrpass_read(ptr: i32, len: i32) -> i64 {
    out(Board::parse(input(ptr, len)).map(|b| Value::from(b.values()).to_string().into_bytes()))
}

#[unsafe(no_mangle)]
pub extern "C" fn ctrpass_write(body: i32, body_len: i32, values: i32, values_len: i32) -> i64 {
    out((|| {
        let mut board = Board::parse(input(body, body_len))?;
        board.apply(&json_input(values, values_len)?)?;
        Ok(board.to_bytes())
    })())
}

#[unsafe(no_mangle)]
pub extern "C" fn ctrpass_problems(ptr: i32, len: i32) -> i64 {
    out(Ok(json!(problems(input(ptr, len)))
        .to_string()
        .into_bytes()))
}
