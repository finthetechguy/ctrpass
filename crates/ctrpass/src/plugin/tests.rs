use serde_json::Value;

use super::*;
use crate::cec::icon::Icon;
use crate::identity::Identity;
use crate::mii::Record;
use crate::title::{self, Context, from_text, hex};

const KEY: crate::keys::Key = [0x31; 16];

fn ctx(id: &Identity) -> Context<'_> {
    Context {
        record_key: Some(&KEY),
        identity: Some(id),
    }
}

fn plaza() -> &'static Title {
    title::find("Mii Plaza").unwrap()
}

fn set(t: &Title, pairs: &[(&str, &str)]) -> Values {
    pairs
        .iter()
        .map(|(k, v)| {
            (
                k.to_string(),
                from_text(title::field(t, k).unwrap(), v).unwrap(),
            )
        })
        .collect()
}

#[test]
fn built_in_plugins_load() {
    let r = Registry::load(None);
    assert!(r.errors().is_empty(), "{:?}", r.errors());
    assert_eq!(r.all().len(), BUILT_IN.len());
    for t in r.all() {
        assert_eq!(t.source(), &Source::BuiltIn);
    }
}

#[test]
fn plaza_create_read_write() {
    let id = Identity::from_seed(&[3; 32]);
    let t = plaza();
    let body = t.create(&ctx(&id)).unwrap();
    assert_eq!(body.len(), 0x7280);
    let v = t.read(&body, &ctx(&id)).unwrap();
    assert_eq!(v["name"], "ctrpass");
    assert_eq!(v["mii_id"], u64::from(id.mii_id));
    assert_eq!(v["hat"], u64::from(id.hat));
    assert_eq!(v["featured"], 0x0004_0000_0000_0000u64);
    assert_eq!(v["icon"], hex(&Icon::standard(48).encode()));
    assert!(t.problems(&body).is_empty(), "{:?}", t.problems(&body));
    // Every value read back is accepted, and writing it all changes nothing.
    assert_eq!(t.write(&body, &v, &ctx(&id)).unwrap(), body);

    let mut edit = set(
        t,
        &[
            ("greeting", "Hello"),
            ("name", "Ada"),
            ("hat", "12"),
            ("country", "Here"),
            ("featured", "0004000000123400"),
        ],
    );
    edit.insert(
        "icon".into(),
        Value::from(hex(&Icon::filled(48, 0xf800).encode())),
    );
    let body2 = t.write(&body, &edit, &ctx(&id)).unwrap();
    let v2 = t.read(&body2, &ctx(&id)).unwrap();
    assert_eq!(
        (&v2["greeting"], &v2["name"], &v2["hat"], &v2["country"]),
        (&"Hello".into(), &"Ada".into(), &12.into(), &"Here".into())
    );
    assert_eq!(v2["featured"], 0x0004_0000_0012_3400u64);
    assert_eq!(v2["small_icon"], hex(&Icon::filled(24, 0xf800).encode()));
    assert_eq!(v2["region"], v["region"]);
    assert!(t.problems(&body2).is_empty());
    // Country is set in every language.
    assert_eq!(
        &body2[0x3760 + 15 * 0x40..0x3760 + 15 * 0x40 + 8],
        b"H\0e\0r\0e\0"
    );
}

#[test]
fn plaza_edits_touch_only_their_fields() {
    let id = Identity::from_seed(&[5; 32]);
    let t = plaza();
    let before = t.create(&ctx(&id)).unwrap();
    let after = t
        .write(
            &before,
            &set(t, &[("greeting", "Hi there"), ("hat", "12")]),
            &ctx(&id),
        )
        .unwrap();
    let changed: Vec<usize> = (0..before.len())
        .filter(|&i| before[i] != after[i])
        .collect();
    assert!(!changed.is_empty());
    assert!(
        changed
            .iter()
            .all(|&i| (0x3f70..0x3f92).contains(&i) || i == 0x3f66),
        "{changed:x?}"
    );
}

#[test]
fn plaza_refuses_bad_values() {
    let id = Identity::from_seed(&[4; 32]);
    let t = title::find("00020800").unwrap();
    let body = t.create(&ctx(&id)).unwrap();
    for (k, text) in [
        ("hat", "69"),
        ("name", "a%b"),
        ("greeting", "x".repeat(17).as_str()),
    ] {
        assert!(from_text(title::field(t, k).unwrap(), text).is_err(), "{k}");
    }
    let mut v = Values::new();
    v.insert("mii_id".into(), Value::from(0x20u64));
    assert!(t.write(&body, &v, &ctx(&id)).is_err());
    v.clear();
    v.insert("nope".into(), Value::from(1));
    assert!(t.write(&body, &v, &ctx(&id)).is_err());
    let no_key = Context {
        record_key: None,
        identity: Some(&id),
    };
    assert!(t.read(&body, &no_key).is_err());
    assert!(t.read(&[0; 0x60f0], &ctx(&id)).is_err());
    assert!(t.create(&Context::default()).is_err());
}

#[test]
fn plaza_problems() {
    let id = Identity::from_seed(&[6; 32]);
    let t = plaza();
    let mut body = t.create(&ctx(&id)).unwrap();
    body[0x3f66] = 69;
    body[0x3f68] = 20;
    body[..8].fill(0);
    assert_eq!(t.problems(&body).len(), 3, "{:?}", t.problems(&body));
    body[0x36bc] = 0;
    assert!(
        t.problems(&body)
            .contains(&"the body holds no Mii".to_string())
    );
}

#[test]
fn plaza_takes_a_mii_record() {
    let id = Identity::from_seed(&[7; 32]);
    let t = plaza();
    let body = t.create(&ctx(&id)).unwrap();
    let mii = Record::new("Visitor", 0x1234_5698, [0, 1, 2, 3, 4, 5], 3, 0).unwrap();
    let mut v = Values::new();
    v.insert("mii".into(), hex(mii.as_bytes()).into());
    v.insert("creator".into(), "Me".into());
    let body = t.write(&body, &v, &ctx(&id)).unwrap();
    let back = t.read(&body, &ctx(&id)).unwrap();
    assert_eq!(back["name"], "Visitor");
    assert_eq!(back["creator"], "Me");
    assert_eq!(back["mii_id"], 0x1234_5698u64);
    assert_eq!(back["creator_mac"], "000102030405");

    let mut bad = mii.clone();
    bad.set_identity(0x1234_5678 | 0x20, [0; 6]);
    bad.update_crc();
    v.clear();
    v.insert("mii".into(), hex(bad.as_bytes()).into());
    assert!(t.write(&body, &v, &ctx(&id)).is_err());
}

#[test]
fn plaza_message_is_signed() {
    let id = Identity::from_seed(&[8; 32]);
    let t = plaza();
    let key = [0x08; 32];
    let body = t.create(&ctx(&id)).unwrap();
    let c = title::message(t, &body, &key, None, None, None).unwrap();
    assert_eq!((c.title(), c.send_mode(), c.flags()), (0x0002_0800, 0, 3));
    assert!(c.verify(&key));
    assert_eq!(
        title::message_text(&c).as_deref(),
        Some("A visitor arrived")
    );
    assert_eq!(c.body(), &body[..]);
}

const SMALL: &str = r#"
api = 1
title = "00ABCD00"
name = "Small"
version = "0.1"
author = "Someone"
message_text = "Hi"
size = 0x40

[[field]]
key = "note"
label = "Note"
type = "text"
at = 0x10
bytes = 0x10

[[field]]
key = "score"
label = "Score"
type = "number"
at = 0x4
width = 2
big_endian = true

[[computed]]
type = "length"
at = 0x8
width = 1
field = "note"

[[computed]]
type = "crc32"
at = 0x0
start = 0x4
"#;

fn small() -> Title {
    parse_layout(SMALL, Source::BuiltIn).unwrap()
}

#[test]
fn layout_computes_values() {
    let t = small();
    let id = Identity::from_seed(&[1; 32]);
    let body = t.create(&ctx(&id)).unwrap();
    assert!(t.problems(&body).is_empty(), "{:?}", t.problems(&body));
    let body = t
        .write(
            &body,
            &set(&t, &[("note", "abc"), ("score", "258")]),
            &Context::default(),
        )
        .unwrap();
    assert_eq!(&body[4..6], &[1, 2]);
    assert_eq!(body[8], 3);
    assert!(t.problems(&body).is_empty());
    let mut bad = body.clone();
    bad[0x10] = b'x';
    assert_eq!(t.problems(&bad), vec!["the checksum at 0x0 does not match"]);
    let v = t.read(&body, &Context::default()).unwrap();
    assert_eq!((&v["note"], &v["score"]), (&"abc".into(), &258.into()));
}

#[test]
fn layout_errors_are_clear() {
    let cases = [
        ("api = 1", "api = 2", "plugin API 2"),
        ("at = 0x10", "at = 0x38", "lies past the end"),
        ("type = \"text\"", "type = \"words\"", "unknown"),
        ("key = \"score\"", "key = \"note\"", "used twice"),
        ("width = 2", "width = 3", "width 3"),
        ("message_text", "colour = 1\nmessage_text", "unknown field"),
        ("start = 0x4", "start = 0x0", "inside its own range"),
        ("field = \"note\"", "field = \"score\"", "not a text field"),
    ];
    for (from, to, want) in cases {
        let text = SMALL.replacen(from, to, 1);
        let e = parse_layout(&text, Source::BuiltIn)
            .unwrap_err()
            .to_string();
        assert!(e.contains(want), "{to}: {e}");
    }
    let bad_create = format!("{SMALL}\n[create]\nvalues = {{ score = 0x10000 }}\n");
    assert!(parse_layout(&bad_create, Source::BuiltIn).is_err());
}

#[test]
fn folder_plugins_load_beside_built_in_ones() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("small.toml"), SMALL).unwrap();
    std::fs::write(dir.path().join("broken.toml"), "api = 1\n").unwrap();
    std::fs::write(
        dir.path().join("twice.toml"),
        SMALL.replace("Small", "Twice"),
    )
    .unwrap();
    std::fs::write(dir.path().join("readme.txt"), "not a plugin").unwrap();
    let plaza = include_str!("../../titles/mii-plaza.toml").replace("1.0.0", "2.0.0");
    std::fs::write(dir.path().join("plaza.toml"), plaza).unwrap();

    let r = Registry::load(Some(dir.path()));
    assert_eq!(r.all().len(), 2);
    let small = r.find("small").unwrap();
    assert_eq!(small.source().path(), Some(&*dir.path().join("small.toml")));
    assert_eq!(r.by_id(0x0002_0800).unwrap().version(), "2.0.0");
    assert_eq!(r.warnings().len(), 1);
    let failed: Vec<_> = r.errors().iter().map(|e| e.file.clone()).collect();
    assert_eq!(
        failed,
        vec![
            dir.path().join("broken.toml"),
            dir.path().join("twice.toml")
        ]
    );
    assert!(
        Registry::load(Some(&dir.path().join("none")))
            .errors()
            .is_empty()
    );
}

const MANIFEST: &str = r#"{"api":1,"title":"00ABCD01","name":"Module","version":"1","author":"Someone","message_text":"Hi","fields":[{"key":"score","label":"Score","type":"number","max":255}]}"#;

fn wat_bytes(b: &[u8]) -> String {
    b.iter().map(|b| format!("\\{b:02x}")).collect()
}

/// A module with a four-byte body whose read function is `read`.
fn module(read: &str, imports: &str) -> Vec<u8> {
    let manifest = [&[0u8][..], MANIFEST.as_bytes()].concat();
    let text = format!(
        r#"(module {imports}
          (memory (export "memory") 1)
          (global $heap (mut i32) (i32.const 4096))
          (data (i32.const 64) "{manifest}")
          (data (i32.const 2048) "{values}")
          (data (i32.const 2100) "{created}")
          (data (i32.const 2200) "{problems}")
          (data (i32.const 2300) "{failed}")
          (data (i32.const 2400) "{wrong}")
          (data (i32.const 2500) "{over}")
          (func $out (param i32 i32) (result i64)
            (i64.or (i64.shl (i64.extend_i32_u (local.get 0)) (i64.const 32))
                    (i64.extend_i32_u (local.get 1))))
          (func (export "ctrpass_alloc") (param i32) (result i32) (local i32)
            (local.set 1 (i32.add (global.get $heap) (i32.const 1)))
            (global.set $heap (i32.add (local.get 1) (local.get 0)))
            (local.get 1))
          (func (export "ctrpass_manifest") (result i64)
            (call $out (i32.const 64) (i32.const {manifest_len})))
          (func (export "ctrpass_create") (param i32 i32) (result i64)
            (call $out (i32.const 2100) (i32.const 5)))
          (func (export "ctrpass_read") (param i32 i32) (result i64) {read})
          (func (export "ctrpass_write") (param i32 i32 i32 i32) (result i64)
            (i32.store8 (i32.sub (local.get 0) (i32.const 1)) (i32.const 0))
            (call $out (i32.sub (local.get 0) (i32.const 1)) (i32.add (local.get 1) (i32.const 1))))
          (func (export "ctrpass_problems") (param i32 i32) (result i64)
            (call $out (i32.const 2200) (i32.const 3))))"#,
        manifest = wat_bytes(&manifest),
        manifest_len = manifest.len(),
        values = wat_bytes(b"\0{\"score\":7}"),
        created = wat_bytes(b"\0\x07\0\0\0"),
        problems = wat_bytes(b"\0[]"),
        failed = wat_bytes(b"\x01bad body"),
        wrong = wat_bytes(b"\0{\"score\":\"x\"}"),
        over = wat_bytes(b"\0{\"score\":300}"),
    );
    wat::parse_str(text).unwrap()
}

const READ: &str = "(call $out (i32.const 2048) (i32.const 12))";

#[test]
fn module_round_trip() {
    let t = parse_module(&module(READ, ""), Source::BuiltIn).unwrap();
    assert_eq!(
        (t.id(), t.name(), t.fields().len()),
        (0x00AB_CD01, "Module", 1)
    );
    let id = Identity::from_seed(&[2; 32]);
    let body = t.create(&ctx(&id)).unwrap();
    assert_eq!(body, [7, 0, 0, 0]);
    let v = t.read(&body, &Context::default()).unwrap();
    assert_eq!(v["score"], 7);
    assert_eq!(t.write(&body, &v, &Context::default()).unwrap(), body);
    assert!(t.problems(&body).is_empty());
    let mut over = Values::new();
    over.insert("score".into(), 256.into());
    assert!(t.write(&body, &over, &Context::default()).is_err());
}

#[test]
fn module_failures_stay_contained() {
    let body = [7, 0, 0, 0];
    let read_error = |read: &str| {
        let t = parse_module(&module(read, ""), Source::BuiltIn).unwrap();
        t.read(&body, &Context::default()).unwrap_err().to_string()
    };
    assert!(read_error("unreachable").contains("failed"));
    assert!(read_error("(loop $l (br $l)) (i64.const 0)").contains("fuel"));
    assert!(read_error("(call $out (i32.const 2300) (i32.const 9))").contains("bad body"));
    assert!(read_error("(call $out (i32.const 2400) (i32.const 14))").contains("does not fit"));
    assert!(read_error("(i64.const 0)").contains("0 bytes"));

    let t = parse_module(
        &module("(call $out (i32.const 2500) (i32.const 14))", ""),
        Source::BuiltIn,
    )
    .unwrap();
    assert_eq!(t.problems(&body), vec!["score: 300 is outside 0..=255"]);

    let e = parse_module(
        &module(READ, r#"(import "env" "f" (func))"#),
        Source::BuiltIn,
    )
    .unwrap_err()
    .to_string();
    assert!(e.contains("import"), "{e}");
    assert!(parse_module(b"not wasm", Source::BuiltIn).is_err());
}
