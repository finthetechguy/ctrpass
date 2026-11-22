use super::*;
use crate::cec::container::Timestamp;
use crate::cec::container::tests::{KEY, sample};
use crate::keys::TitleKey;
use std::fs;

const PLAZA: u32 = 0x0002_0800;
const OTHER: u32 = 0x0003_0600;

fn info(title: u32, key: &TitleKey) -> Vec<u8> {
    let mut b = vec![0u8; 0x60];
    b[..2].copy_from_slice(b"cc");
    b[4..8].copy_from_slice(&title.to_le_bytes());
    b[0x10..0x30].copy_from_slice(key);
    b
}

fn name(n: &str) -> Vec<u8> {
    n.encode_utf16()
        .flat_map(u16::to_le_bytes)
        .chain([0, 0])
        .collect()
}

fn image() -> Vec<u8> {
    let mut padded = sample(PLAZA, 2).as_bytes().to_vec();
    padded.resize(padded.len() + 0x40, 0);
    savefs::build(
        &[
            (0, "CEC"),
            (1, "00020800"),
            (2, "OutBox__"),
            (2, "InBox___"),
        ],
        &[
            (1, "MBoxList____", &[0x68, 0x68]),
            (2, "MBoxInfo____", &info(PLAZA, &KEY)),
            (2, "MBoxData.010", &name("Mii Plaza")),
            (3, "_out", sample(PLAZA, 1).as_bytes()),
            (4, "_in", &padded),
            (4, "_broken", b"\x60\x60junk"),
        ],
    )
}

#[test]
fn reads_a_save_image() {
    let src = source::read_save(&image()).unwrap();
    assert_eq!(
        src.titles,
        [source::TitleInfo {
            title: PLAZA,
            name: Some("Mii Plaza".into()),
            key: Some(KEY),
        }]
    );
    let found: Vec<_> = src
        .messages
        .iter()
        .map(|f| (f.container.message_id(), f.mailbox, f.path.as_str()))
        .collect();
    assert_eq!(
        found,
        [
            (1, Some(Mailbox::Outbox), "CEC/00020800/OutBox__/_out"),
            (2, Some(Mailbox::Inbox), "CEC/00020800/InBox___/_in"),
        ]
    );
    assert_eq!(src.skipped.len(), 1);
}

#[test]
fn imports_once() {
    let d = tempfile::tempdir().unwrap();
    let mut store = Store::open(d.path()).unwrap();
    let src = source::read_save(&image()).unwrap();
    let got = import(&mut store, &src, None).unwrap();
    assert_eq!(
        (got.added, got.duplicates, got.titles.clone()),
        (2, 0, vec![PLAZA])
    );
    assert_eq!(store.title_key(PLAZA), Some(KEY));
    assert_eq!(store.title_name(PLAZA).as_deref(), Some("Mii Plaza"));
    assert_eq!(
        store
            .messages(Some(PLAZA), Some(Mailbox::Outbox))
            .unwrap()
            .len(),
        1
    );
    let again = import(&mut store, &src, None).unwrap();
    assert_eq!((again.added, again.duplicates), (0, 2));
}

#[test]
fn import_keeps_the_held_key() {
    let d = tempfile::tempdir().unwrap();
    let mut store = Store::open(d.path()).unwrap();
    let other = [9u8; 32];
    store.add_title(PLAZA, None, Some(&other), false).unwrap();
    let got = import(&mut store, &source::read_save(&image()).unwrap(), None).unwrap();
    assert_eq!(got.key_conflicts, [PLAZA]);
    assert_eq!(got.added, 0);
    assert_eq!(got.refused.len(), 2);
    assert_eq!(store.title_key(PLAZA), Some(other));
}

#[test]
fn delivers_into_a_cec_folder() {
    let d = tempfile::tempdir().unwrap();
    {
        let mut store = Store::open(d.path()).unwrap();
        store.add_title(PLAZA, None, Some(&KEY), false).unwrap();
    }
    let folder = CecFolder::open(d.path()).unwrap();
    assert_eq!(folder.path(), d.path().join("CEC"));
    let when = Timestamp::from_unix(1_700_000_000, 0);
    let msg = sample(PLAZA, 5);
    assert_eq!(folder.deliver(&msg, when, true).unwrap(), Delivery::Added);
    assert_eq!(folder.deliver(&msg, when, false).unwrap(), Delivery::Added);
    assert_eq!(
        folder.deliver(&msg, when, false).unwrap(),
        Delivery::Duplicate
    );
    assert_eq!(
        folder.deliver(&sample(OTHER, 6), when, false).unwrap(),
        Delivery::NoBox
    );
    let mut bad = sample(PLAZA, 7).as_bytes().to_vec();
    let n = bad.len();
    bad[n - 1] ^= 1;
    let bad = crate::cec::Container::parse(&bad).unwrap();
    assert_eq!(
        folder.deliver(&bad, when, false).unwrap(),
        Delivery::Invalid
    );

    let inbox = d.path().join("CEC/00020800/InBox___");
    let boxinfo = fs::read(inbox.join("BoxInfo_____")).unwrap();
    assert_eq!(boxinfo.len(), 0x20 + 0x70);
    assert_eq!(
        u32::from_le_bytes(boxinfo[0x14..0x18].try_into().unwrap()),
        1
    );
    let filed = fs::read(inbox.join(crate::store::message_file_name(5))).unwrap();
    assert_eq!(&filed[0x36..0x38], &[1, 1]);
    assert_eq!(&filed[..0x70], &boxinfo[0x20..]);
    let info = fs::read(d.path().join("CEC/00020800/MBoxInfo____")).unwrap();
    assert_eq!(&info[0x40..0x42], &[1, 1]);

    let back = source::read(d.path()).unwrap();
    assert_eq!(back.messages.len(), 1);
    assert_eq!(back.messages[0].mailbox, Some(Mailbox::Inbox));
    assert_eq!(back.titles[0].key, Some(KEY));
}

#[test]
fn writes_raw_files() {
    let d = tempfile::tempdir().unwrap();
    let paths = folder::write_raw(&[sample(PLAZA, 1), sample(OTHER, 2)], d.path()).unwrap();
    assert_eq!(paths.len(), 2);
    let back = source::read(d.path()).unwrap();
    assert_eq!(back.messages.len(), 2);
    assert_eq!(back.messages[0].mailbox, None);
}
