use std::time::Instant;

use super::*;
use crate::cec::container::tests::{KEY, sample};
use crate::sim::{Behaviour, Console, KEYS};

const US: Mac = [0x9a, 0x68, 0x0a, 1, 2, 3];
const T1: u32 = 0x0002_0800;
const T3: u32 = 0x0008_c500;

fn store(receive: bool) -> (tempfile::TempDir, Store) {
    let d = tempfile::tempdir().unwrap();
    let mut s = Store::open(d.path()).unwrap();
    s.add_title(T1, None, Some(&KEY), false).unwrap();
    s.add_message(&sample(T1, 1), Mailbox::Outbox, None, None)
        .unwrap();
    s.add_title(T3, None, None, false).unwrap();
    s.set_receive(T3, receive).unwrap();
    (d, s)
}

fn console(now: Instant) -> Console {
    let services = [T1, T3]
        .iter()
        .map(|&t| Service::new(t, 0x30, vec![]).unwrap())
        .collect();
    Console::new(Behaviour::Hosts, &[sample(T3, 3)], services, now)
}

fn pass(store: &mut Store, console: &mut Console) -> Report {
    let mut book = store.peer_book().unwrap();
    let services = local_services(store)
        .unwrap()
        .into_iter()
        .map(|(s, _)| s)
        .collect();
    let mut host = StoreHost::new(store, &mut book);
    let config = link::Config {
        role: Role::Lead,
        mac: US,
        token: [9; 8],
        services,
        channels: vec![1],
        dwell: link::DWELL,
        linger: link::LINGER,
    };
    let mut link = Link::new(config, &Keys::parse(KEYS).unwrap(), Instant::now());
    crate::sim::run(&mut link, console, &mut host, 0, 0);
    assert!(link.is_done());
    host.finish(link.outcome()).unwrap()
}

#[test]
fn advertises_outbox_and_receive_titles() {
    let (_d, s) = store(true);
    let local = local_services(&s).unwrap();
    assert_eq!(
        local
            .iter()
            .map(|(s, src)| (s.title, s.mode(), *src))
            .collect::<Vec<_>>(),
        vec![(T1, 1, Source::Outbox), (T3, RECEIVE_MODE, Source::Receive)]
    );
}

#[test]
fn exchange_files_and_filters() {
    let (_d, mut s) = store(true);
    let mut c = console(Instant::now());
    let report = pass(&mut s, &mut c);
    assert_eq!(report.peer, Some(c.mac));
    assert_eq!(report.sent, vec![(T1, 1)]);
    assert_eq!(report.received, vec![(T3, 3)]);
    assert!(report.not_filed.is_empty());
    let inbox = s.messages(Some(T3), Some(Mailbox::Inbox)).unwrap();
    assert_eq!(inbox.len(), 1);
    assert_eq!(
        s.origin(Mailbox::Inbox, 3).map(|p| &p.origin),
        Some(&Origin::Peer(c.mac))
    );
    let mut book = s.peer_book().unwrap();
    assert!(book.is_filtered(&c.mac) && book.already_sent(&c.mac, 1));
    let mut host = StoreHost::new(&mut s, &mut book);
    assert!(host.choose(c.mac, &[]).is_none());
    assert_eq!(host.declined, Some("already exchanged"));
}

#[test]
fn title_not_received_is_refused() {
    let (_d, mut s) = store(false);
    let mut c = console(Instant::now());
    let report = pass(&mut s, &mut c);
    assert_eq!(report.sent, vec![(T1, 1)]);
    assert!(report.received.is_empty());
    assert!(
        s.messages(Some(T3), Some(Mailbox::Inbox))
            .unwrap()
            .is_empty()
    );
}

#[test]
fn received_messages_are_not_sent_on() {
    let d = tempfile::tempdir().unwrap();
    let mut s = Store::open(d.path()).unwrap();
    s.add_message(
        &sample(T1, 1),
        Mailbox::Outbox,
        None,
        Some(Origin::Peer([2; 6])),
    )
    .unwrap();
    let mut book = s.peer_book().unwrap();
    let peer = [Service::new(T1, 0x30, vec![]).unwrap()];
    let mut host = StoreHost::new(&mut s, &mut book);
    assert!(host.choose([4; 6], &peer).is_none());
    assert_eq!(host.declined, Some("nothing to trade"));
}
