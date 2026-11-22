use super::*;
use crate::cec::container::tests::{KEY, sample};

const PLAZA: u32 = 0x0002_0800;
const OTHER: u32 = 0x0003_0600;
const PEER: Mac = [0x96, 0x58, 0xcb, 1, 2, 3];

fn dir() -> tempfile::TempDir {
    tempfile::tempdir().unwrap()
}

fn ids(store: &Store, title: u32, mailbox: Mailbox) -> Vec<u64> {
    let mut v: Vec<u64> = store
        .messages(Some(title), Some(mailbox))
        .unwrap()
        .iter()
        .map(|s| s.container.message_id())
        .collect();
    v.sort();
    v
}

#[test]
fn file_names_match_console() {
    assert_eq!(message_file_name(0), "_AAAAAAAAAAA");
    assert_eq!(message_file_name(u64::MAX), "_----------8");
    assert_eq!(message_file_name(0x0102_0304_0506_0708), "_CAcGBQQDAgE");
}

#[test]
fn layout_round_trips() {
    let d = dir();
    {
        let mut s = Store::open(d.path()).unwrap();
        s.add_title(PLAZA, Some("Mii Plaza"), Some(&KEY), false)
            .unwrap();
        s.add_message(&sample(PLAZA, 1), Mailbox::Outbox, None, None)
            .unwrap();
        s.set_receive(PLAZA, true).unwrap();
    }
    let list = fs::read(d.path().join("CEC/MBoxList____")).unwrap();
    assert_eq!(parse_list(&list).unwrap(), vec![PLAZA]);
    let info = fs::read(d.path().join("CEC/00020800/OutBox__/BoxInfo_____")).unwrap();
    assert_eq!(u32_at(&info, 0x14), 1);
    let s = Store::open(d.path()).unwrap();
    assert_eq!(s.title_name(PLAZA).as_deref(), Some("Mii Plaza"));
    assert_eq!(s.title_key(PLAZA), Some(KEY));
    assert_eq!(s.receive_titles(), vec![PLAZA]);
    assert_eq!(ids(&s, PLAZA, Mailbox::Outbox), vec![1]);
    assert_eq!(s.verify(&sample(PLAZA, 1)), Some(true));
}

#[test]
fn second_open_is_refused() {
    let d = dir();
    let s = Store::open(d.path()).unwrap();
    assert!(matches!(Store::open(d.path()), Err(Error::Busy(_))));
    drop(s);
    Store::open(d.path()).unwrap();
}

#[test]
fn keys_are_checked() {
    let d = dir();
    let mut s = Store::open(d.path()).unwrap();
    s.add_title(PLAZA, None, Some(&KEY), false).unwrap();
    let other = [9u8; 32];
    assert!(matches!(
        s.add_title(PLAZA, None, Some(&other), false),
        Err(Error::KeyConflict(PLAZA))
    ));
    assert!(matches!(
        s.add_message(&sample(PLAZA, 2), Mailbox::Outbox, Some(&other), None),
        Err(Error::Authentication)
    ));
    s.add_title(PLAZA, None, Some(&other), true).unwrap();
    assert_eq!(s.title_key(PLAZA), Some(other));
    assert_eq!(s.verify(&sample(PLAZA, 2)), Some(false));
}

#[test]
fn inbox_drops_oldest() {
    let d = dir();
    let mut s = Store::open(d.path()).unwrap();
    for i in 1..=INBOX_MAX_MESSAGES as u64 + 3 {
        let c = sample(OTHER, i);
        let dropped = s
            .add_message(&c, Mailbox::Inbox, None, Some(Origin::Peer(PEER)))
            .unwrap();
        assert_eq!(dropped, usize::from(i > INBOX_MAX_MESSAGES as u64));
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    let held = ids(&s, OTHER, Mailbox::Inbox);
    assert_eq!(held.len(), INBOX_MAX_MESSAGES);
    assert_eq!(held[0], 4);
    assert!(s.origin(Mailbox::Inbox, 1).is_none());
    assert_eq!(
        s.origin(Mailbox::Inbox, 4).map(|p| &p.origin),
        Some(&Origin::Peer(PEER))
    );
}

#[test]
fn arrivals_fit_whole_or_not_at_all() {
    let d = dir();
    let mut s = Store::open(d.path()).unwrap();
    for i in 1..=INBOX_MAX_MESSAGES as u64 - 2 {
        s.add_message(&sample(OTHER, i), Mailbox::Inbox, None, None)
            .unwrap();
    }
    let (_, slots) = s.inbox_room(OTHER).unwrap();
    assert_eq!(slots, 2);
    let three: Vec<Container> = (100..103).map(|i| sample(OTHER, i)).collect();
    assert!(matches!(
        s.file_inbound(&three, PEER),
        Err(Error::NoRoom(_))
    ));
    assert_eq!(ids(&s, OTHER, Mailbox::Inbox).len(), INBOX_MAX_MESSAGES - 2);
    let filed = s.file_inbound(&three[..2], PEER).unwrap();
    assert_eq!(filed.filed, vec![100, 101]);
    assert!(s.file_inbound(&three[..1], PEER).is_ok());
    let room = |t| s.inbox_room(t).unwrap();
    let admitted = admit(&[(OTHER, 100, 1), (PLAZA, 100, 3)], room);
    assert_eq!(admitted.keys().copied().collect::<Vec<_>>(), vec![PLAZA]);
}

#[test]
fn replace_outbox_keeps_one() {
    let d = dir();
    let mut s = Store::open(d.path()).unwrap();
    for i in 1..=2 {
        s.add_message(&sample(OTHER, i), Mailbox::Outbox, None, None)
            .unwrap();
    }
    s.add_message(&sample(PLAZA, 3), Mailbox::Outbox, None, None)
        .unwrap();
    let n = s
        .replace_outbox(&sample(OTHER, 5), None, Some(Origin::Local("made".into())))
        .unwrap();
    assert_eq!(n, 2);
    assert_eq!(ids(&s, OTHER, Mailbox::Outbox), vec![5]);
    assert_eq!(ids(&s, PLAZA, Mailbox::Outbox), vec![3]);
    assert_eq!(s.replace_outbox(&sample(OTHER, 5), None, None).unwrap(), 0);
}

#[test]
fn remove_title_forgets_everything() {
    let d = dir();
    let mut s = Store::open(d.path()).unwrap();
    s.add_title(PLAZA, Some("x"), Some(&KEY), false).unwrap();
    s.add_message(&sample(PLAZA, 1), Mailbox::Inbox, None, None)
        .unwrap();
    s.set_receive(PLAZA, true).unwrap();
    assert_eq!(s.remove_title(PLAZA).unwrap(), 1);
    assert!(s.titles().is_empty() && s.receive_titles().is_empty());
    assert!(s.title_key(PLAZA).is_none());
    assert!(!d.path().join("CEC/00020800").exists());
    assert!(s.set_receive(PLAZA, true).is_err());
}

#[test]
fn title_limit() {
    let d = dir();
    let mut s = Store::open(d.path()).unwrap();
    for i in 0..MAX_TITLES as u32 {
        s.add_title(0x0010_0000 + i, None, None, false).unwrap();
    }
    assert!(s.add_title(0x0020_0000, None, None, false).is_err());
    assert!(!d.path().join("CEC/00200000").exists());
}

#[test]
fn damage_is_set_aside() {
    let d = dir();
    {
        let mut s = Store::open(d.path()).unwrap();
        s.add_title(OTHER, None, Some(&KEY), false).unwrap();
        s.add_message(&sample(OTHER, 1), Mailbox::Inbox, None, None)
            .unwrap();
        s.add_message(&sample(OTHER, 2), Mailbox::Inbox, None, None)
            .unwrap();
        s.add_message(&sample(PLAZA, 3), Mailbox::Inbox, None, None)
            .unwrap();
    }
    let inbox = d.path().join("CEC/00030600/InBox___");
    fs::write(inbox.join(message_file_name(2)), [0u8; 8]).unwrap();
    fs::write(inbox.join(".x.tmp"), b"").unwrap();
    fs::write(d.path().join("CEC/MBoxList____"), b"").unwrap();
    fs::write(d.path().join("state.json"), b"{\"kee").unwrap();
    let s = Store::open(d.path()).unwrap();
    assert_eq!(s.damaged().len(), 3);
    assert!(s.damaged().iter().all(|x| x.moved_to.exists()));
    assert!(!inbox.join(".x.tmp").exists());
    assert_eq!(s.titles(), &[PLAZA, OTHER]);
    assert_eq!(ids(&s, OTHER, Mailbox::Inbox), vec![1]);
    assert_eq!(s.title_key(OTHER), Some(KEY));
    assert_eq!(s.title_key(PLAZA), None);
    let info = fs::read(inbox.join("BoxInfo_____")).unwrap();
    assert_eq!(u32_at(&info, 0x14), 1);
}

#[test]
fn seed_and_services_persist() {
    let d = dir();
    let record = Service::new(PLAZA, 0x30, vec![]).unwrap();
    let seed = {
        let mut s = Store::open(d.path()).unwrap();
        s.add_title(PLAZA, None, None, false).unwrap();
        s.set_service(PLAZA, Some(&record)).unwrap();
        assert!(s.set_service(OTHER, Some(&record)).is_err());
        s.seed().unwrap()
    };
    let mut s = Store::open(d.path()).unwrap();
    assert_eq!(s.seed().unwrap(), seed);
    assert_eq!(s.service(PLAZA), Some(record));
    s.set_seed(&[1; 32]).unwrap();
    assert_eq!(
        s.identity().unwrap(),
        crate::identity::Identity::from_seed(&[1; 32])
    );
    let other = dir();
    assert_ne!(Store::open(other.path()).unwrap().seed().unwrap(), seed);
}

#[test]
fn peer_book() {
    let d = dir();
    let s = Store::open(d.path()).unwrap();
    let mut book = s.peer_book().unwrap();
    assert!(book.due(&PEER, 1000));
    book.note_attempt(&PEER, 1000).unwrap();
    assert_eq!(book.wait(&PEER, 1010), peers::RETRY - 10);
    assert!(!book.due(&PEER, 1010));
    assert!(book.due(&PEER, 1000 + peers::RETRY));
    book.record(&PEER, &[1, 2], &[7], 1100).unwrap();
    assert!(book.is_filtered(&PEER) && !book.due(&PEER, 99_999));
    assert!(book.already_sent(&PEER, 2) && book.came_from(&PEER, 7));
    let book = s.peer_book().unwrap();
    assert_eq!(book.peer(&PEER).map(|p| p.exchanges), Some(1));
    let mut book = book;
    for i in 0..peers::FILTER_MAX as u8 {
        book.record(&[2, 0, 0, 0, 1, i], &[], &[], 1200).unwrap();
    }
    assert_eq!(book.filter().len(), peers::FILTER_MAX);
    assert!(!book.is_filtered(&PEER));
    book.block(&PEER, "", 1300).unwrap();
    assert!(!book.due(&PEER, 1300));
    assert_eq!(book.forget(None).unwrap(), peers::FILTER_MAX + 1);
    assert!(book.unblock(&PEER).unwrap());
}
