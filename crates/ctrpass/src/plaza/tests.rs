use super::*;
use crate::keys::TitleKey;

const RECORD_KEY: Key = [0x31; 16];
const TITLE_KEY: TitleKey = [0x08; 32];

fn mii() -> Record {
    Record::new("Visitor", 0x1234_5698, [0, 1, 2, 3, 4, 5], 3, 0).unwrap()
}

fn plaza() -> Plaza {
    Plaza::new(&RECORD_KEY, &mii()).unwrap()
}

#[test]
fn new_body_reads_back() {
    let p = plaza();
    assert_eq!(p.as_bytes().len(), BODY_LEN);
    assert_eq!(p.mii(&RECORD_KEY).unwrap(), mii());
    assert_eq!(p.featured_id(), NO_FEATURED_TITLE);
    assert_eq!(p.presence(), PRESENT);
    assert_eq!(p.icons().1, Icon::standard(48));
    assert!(p.problems().is_empty(), "{:?}", p.problems());
    assert!(p.history().is_empty());
}

#[test]
fn edits_touch_only_their_fields() {
    let before = plaza();
    let mut p = before.clone();
    p.set_greeting("Hi there").unwrap();
    p.set_profile(&Profile {
        hat: 12,
        ..p.profile()
    });
    let changed: Vec<usize> = (0..BODY_LEN)
        .filter(|&i| p.as_bytes()[i] != before.as_bytes()[i])
        .collect();
    assert!(
        changed
            .iter()
            .all(|&i| (GREETING..GREETING + GREETING_BYTES).contains(&i) || i == PROFILE + 6)
    );
    assert_eq!(p.greeting(), "Hi there");
    assert_eq!(p.profile().hat, 12);

    let mut q = Plaza::parse(p.as_bytes()).unwrap();
    q.set_names(&Names {
        short: "S".into(),
        long: "L".into(),
        publisher: "P".into(),
    })
    .unwrap();
    q.set_place("Country", "Region").unwrap();
    assert_eq!(q.names(15).publisher, "P");
    assert_eq!(q.place(7), ("Country".into(), "Region".into()));
    assert_eq!(q.greeting(), "Hi there");
}

#[test]
fn limits_are_enforced() {
    let mut p = plaza();
    assert!(p.set_greeting("seventeen letters").is_err());
    assert!(p.set_greeting("sixteen  letters").is_ok());
    p.set_profile(&Profile {
        hat: 69,
        dream: 20,
        ..Profile::default()
    });
    p.set_featured_id(0);
    assert_eq!(p.problems().len(), 3, "{:?}", p.problems());

    let mut bad = mii();
    bad.0[0xc] |= 0x20;
    bad.update_crc();
    assert!(p.set_mii(&RECORD_KEY, &bad).is_err());
    assert!(p.mii(&[0; 16]).is_err());
    assert!(Plaza::parse(&[0; OLD_BODY_LEN]).is_err());
    assert!(Plaza::parse(&[0; 64]).is_err());
}

#[test]
fn history_lists_set_entries() {
    let mut b = plaza().as_bytes().to_vec();
    b[RECENT + 10..RECENT + 14].copy_from_slice(&7u32.to_le_bytes());
    b[RECENT + 14..RECENT + 20].copy_from_slice(&[1; 6]);
    b[RECENT_NAMES + RECENT_NAME_BYTES..RECENT_NAMES + RECENT_NAME_BYTES + 2]
        .copy_from_slice(&[b'A', 0]);
    b[OLDER + 50..OLDER + 54].copy_from_slice(&9u32.to_le_bytes());
    let h = Plaza::parse(&b).unwrap().history();
    assert_eq!(h.len(), 2);
    assert_eq!((h[0].mii_id, h[0].name.as_deref()), (7, Some("A")));
    assert_eq!((h[1].mii_id, h[1].name.as_deref()), (9, None));
}

#[test]
fn message_is_signed_plaza_message() {
    let c = crate::title::message(
        &PlazaTitle,
        plaza().as_bytes(),
        &TITLE_KEY,
        None,
        None,
        None,
    )
    .unwrap();
    assert_eq!((c.title(), c.send_mode(), c.flags()), (TITLE, SEND_MODE, 3));
    assert!(c.verify(&TITLE_KEY));
    assert_eq!(
        c.ext_header(crate::cec::container::EXT_ICON),
        Some(&Icon::standard(40).encode()[..])
    );
    assert_eq!(
        crate::title::message_text(&c).as_deref(),
        Some("A visitor arrived")
    );
    let again = crate::title::message(
        &PlazaTitle,
        c.body(),
        &TITLE_KEY,
        Some(&c),
        None,
        Some("Hi"),
    )
    .unwrap();
    assert_ne!(again.message_id(), c.message_id());
    assert_eq!(crate::title::message_text(&again).as_deref(), Some("Hi"));
    assert_eq!(Plaza::parse(c.body()).unwrap(), plaza());
}
