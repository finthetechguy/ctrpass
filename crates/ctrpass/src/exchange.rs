//! Running passes against a store: choosing what each console gets and filing what it sends.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::Error;
use crate::cec::container::{Container, EXT_SERVICE};
use crate::cec::envelope;
use crate::cec::object::Entry;
use crate::keys::Keys;
use crate::link::{self, Event, Host, Link, Outcome, Plan, Role};
use crate::radio::Radio;
use crate::store::{self, Mailbox, Origin, PeerBook, Store};
use crate::streetpass::service::{self, RECEIVE_MODE, Service};
use crate::wifi::frame::Mac;

/// How long an exchange in progress may run on after a stop is asked for.
const STOP_GRACE: Duration = Duration::from_secs(10);
const RECV_SLICE: Duration = Duration::from_millis(5);

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Where a title's advertised record comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    /// A message waiting to be sent.
    Outbox,
    /// The title only receives.
    Receive,
}

/// The record a message's service extension declares for its own title.
pub fn template(c: &Container) -> Option<Service> {
    let raw = c.ext_header(EXT_SERVICE)?;
    let (mut found, err) = service::decode(raw);
    (err.is_none() && found.len() == 1 && found[0].title == c.title()).then(|| found.remove(0))
}

/// The mode advertised for a sending mode given whether the inbox has room, or `None` to skip the title.
fn effective_mode(mode: u8, room: bool) -> Option<u8> {
    match (room, mode & 3) {
        (true, m) => Some(m),
        (false, 0 | 1) => None,
        (false, _) => Some(2),
    }
}

/// Whether a stored outbox message may be sent: it is authentic and was not received from a console.
fn servable(store: &Store, c: &Container) -> bool {
    store.verify(c) != Some(false)
        && !matches!(
            store
                .origin(Mailbox::Outbox, c.message_id())
                .map(|p| &p.origin),
            Some(Origin::Peer(_))
        )
}

fn outbox_in_order(store: &Store, title: u32) -> Result<Vec<Container>, Error> {
    let mut held: Vec<Container> = store
        .messages(Some(title), Some(Mailbox::Outbox))?
        .into_iter()
        .map(|s| s.container)
        .filter(|c| servable(store, c))
        .collect();
    held.sort_by_key(|c| (c.created(), c.message_id()));
    Ok(held)
}

fn has_room(room: (usize, usize)) -> bool {
    room.0 > 0 && room.1 > 0
}

/// Every record the store advertises, by title in store order.
pub fn local_services(store: &Store) -> Result<Vec<(Service, Source)>, Error> {
    let receiving = store.receive_titles();
    let mut out = Vec::new();
    for &title in store.titles() {
        let room = has_room(store.inbox_room(title)?);
        let held = outbox_in_order(store, title)?;
        if let Some(lead) = held.iter().find(|c| c.send_count() != 0) {
            if let Some(mode) = effective_mode(lead.send_mode(), room) {
                let base = template(lead).unwrap_or(Service::new(title, 0, Vec::new())?);
                out.push((base.with_mode(mode), Source::Outbox));
                continue;
            }
        }
        if receiving.contains(&title) && room {
            let record = match store.service(title) {
                Some(s) => s,
                None => {
                    let mut all: Vec<Container> = store
                        .messages(Some(title), None)?
                        .into_iter()
                        .map(|s| s.container)
                        .collect();
                    all.sort_by_key(|c| std::cmp::Reverse((c.created(), c.message_id())));
                    all.iter()
                        .find_map(template)
                        .unwrap_or(Service::new(title, 0, Vec::new())?)
                        .with_mode(RECEIVE_MODE)
                }
            };
            out.push((record, Source::Receive));
        }
    }
    Ok(out)
}

/// What one pass did, for reporting.
#[derive(Clone, Debug, Default)]
pub struct Report {
    pub peer: Option<Mac>,
    /// Stored message IDs the console took.
    pub sent: Vec<(u32, u64)>,
    /// Local titles the console refused.
    pub refused: Vec<u32>,
    pub received: Vec<(u32, u64)>,
    /// Arrivals that were not filed, with the reason.
    pub not_filed: Vec<(u32, String)>,
}

/// Chooses per console from a store and files what it sends.
pub struct StoreHost<'a> {
    store: &'a mut Store,
    book: &'a mut PeerBook,
    /// Wire message ID to stored message ID for what was offered to the current console.
    offered: BTreeMap<u64, u64>,
    reserved: Option<BTreeMap<u32, (usize, usize)>>,
    pub declined: Option<&'static str>,
}

impl<'a> StoreHost<'a> {
    pub fn new(store: &'a mut Store, book: &'a mut PeerBook) -> Self {
        StoreHost {
            store,
            book,
            offered: BTreeMap::new(),
            reserved: None,
            declined: None,
        }
    }

    fn try_choose(&mut self, peer: Mac, theirs: &[Service]) -> Result<Option<Plan>, Error> {
        self.offered.clear();
        self.reserved = None;
        self.declined = None;
        let now = now_secs();
        if self.book.is_blocked(&peer) {
            self.declined = Some("blocked");
            return Ok(None);
        }
        if self.book.is_filtered(&peer) {
            self.declined = Some("already exchanged");
            return Ok(None);
        }
        if !self.book.due(&peer, now) {
            self.declined = Some("backing off");
            return Ok(None);
        }
        let local = local_services(self.store)?;
        let mut matched: Vec<(Service, Source)> = Vec::new();
        for p in theirs {
            if let Some((s, src)) = local.iter().find(|(s, _)| s.title == p.title) {
                if !matched.iter().any(|(m, _)| m.title == p.title) && s.pairs_with(p).is_ok() {
                    matched.push((s.clone(), *src));
                }
            }
        }
        let mut messages = Vec::new();
        let mut seen = BTreeSet::new();
        for (s, src) in &matched {
            if *src != Source::Outbox {
                continue;
            }
            let mut held = outbox_in_order(self.store, s.title)?;
            held.reverse();
            for c in held {
                let id = c.message_id();
                if !seen.insert(id)
                    || self.book.already_sent(&peer, id)
                    || self.book.came_from(&peer, id)
                {
                    continue;
                }
                let mut fresh = c.clone();
                let wire = u64::from_le_bytes(random8());
                fresh.set_message_id(wire);
                self.offered.insert(wire, id);
                messages.push(fresh);
            }
        }
        let receiving = self.store.receive_titles();
        let mut services: Vec<Service> = Vec::new();
        for (s, _) in &matched {
            let sending = messages.iter().any(|c| c.title() == s.title);
            let inbound = receiving.contains(&s.title) && has_room(self.store.inbox_room(s.title)?);
            if sending || inbound {
                services.push(s.clone());
            }
        }
        if services.is_empty() {
            self.declined = Some("nothing to trade");
            return Ok(None);
        }
        Ok(Some(Plan { services, messages }))
    }

    /// Records the end of a pass and files what arrived.
    pub fn finish(&mut self, outcome: Option<Outcome>) -> Result<Report, Error> {
        let mut report = Report::default();
        let Some(out) = outcome else {
            return Ok(report);
        };
        report.peer = Some(out.peer);
        report.refused = out.refused.clone();
        let mut sent_modes: BTreeMap<u32, BTreeSet<u8>> = BTreeMap::new();
        if out.peer_sent_messages {
            for env in out
                .delivered
                .iter()
                .filter(|o| envelope::title(o).is_some())
            {
                for c in envelope::split(env).unwrap_or_default() {
                    if let Some(&id) = self.offered.get(&c.message_id()) {
                        report.sent.push((c.title(), id));
                        sent_modes
                            .entry(c.title())
                            .or_default()
                            .insert(c.send_mode());
                    }
                }
            }
        }
        let mut arrived: BTreeMap<u32, Vec<Container>> = BTreeMap::new();
        for env in out.received.iter().filter(|o| envelope::title(o).is_some()) {
            let Ok(cs) = envelope::split(env) else {
                report.not_filed.push((
                    envelope::title(env).unwrap_or(0),
                    "malformed envelope".into(),
                ));
                continue;
            };
            for c in cs {
                // A send-mode-0 message is only kept once the local mode-0 send of that title went through.
                if c.send_mode() == 0 && !sent_modes.get(&c.title()).is_some_and(|m| m.contains(&0))
                {
                    report
                        .not_filed
                        .push((c.title(), "send mode 0 without a send in return".into()));
                    continue;
                }
                arrived.entry(c.title()).or_default().push(c);
            }
        }
        for (title, cs) in arrived {
            let size: usize = cs.iter().map(Container::len).sum();
            let admitted = match &self.reserved {
                None => true,
                Some(r) => r
                    .get(&title)
                    .is_some_and(|&(b, n)| cs.len() <= n && size <= b),
            };
            if !admitted {
                report
                    .not_filed
                    .push((title, "more than its offer declared".into()));
                continue;
            }
            match self.store.file_inbound(&cs, out.peer) {
                Ok(filed) => {
                    report
                        .received
                        .extend(filed.filed.iter().map(|&id| (title, id)));
                    for _ in filed.refused {
                        report
                            .not_filed
                            .push((title, "failed its signature".into()));
                    }
                }
                Err(e) => report.not_filed.push((title, e.to_string())),
            }
        }
        let now = now_secs();
        if !report.sent.is_empty() || !report.received.is_empty() {
            let sent: Vec<u64> = report.sent.iter().map(|s| s.1).collect();
            let got: Vec<u64> = report.received.iter().map(|s| s.1).collect();
            self.book.record(&out.peer, &sent, &got, now)?;
        } else if self.declined.is_none() {
            self.book.note_attempt(&out.peer, now)?;
        }
        Ok(report)
    }
}

fn random8() -> [u8; 8] {
    let mut b = [0u8; 8];
    getrandom::fill(&mut b).expect("the system random source failed");
    b
}

impl Host for StoreHost<'_> {
    fn choose(&mut self, peer: Mac, services: &[Service]) -> Option<Plan> {
        self.try_choose(peer, services).unwrap_or_else(|_| {
            self.declined = Some("the store could not be read");
            None
        })
    }

    fn admit(&mut self, offer: &[Entry]) -> BTreeMap<u32, u16> {
        let entries: Vec<(u32, usize, usize)> = offer
            .iter()
            .map(|e| (e.title, e.size as usize, e.count as usize))
            .collect();
        let store = &*self.store;
        let receiving = store.receive_titles();
        let admitted = store::admit(&entries, |t| {
            if receiving.contains(&t) {
                store.inbox_room(t).unwrap_or((0, 0))
            } else {
                (0, 0)
            }
        });
        let codes = offer
            .iter()
            .map(|e| {
                let code = if admitted.contains_key(&e.title) {
                    crate::cec::object::VERDICT_ADMIT
                } else {
                    crate::cec::object::VERDICT_REFUSE
                };
                (e.title, code)
            })
            .collect();
        self.reserved = Some(admitted);
        codes
    }
}

pub struct PassConfig {
    pub role: Role,
    pub channels: Vec<u8>,
    /// How long to wait for a console before the pass ends.
    pub window: Duration,
}

/// Runs one pass on a radio: waits for a console, exchanges with it, and files what arrived.
pub fn run_pass(
    radio: &mut dyn Radio,
    store: &mut Store,
    keys: &Keys,
    pass: &PassConfig,
    stop: &AtomicBool,
    mut on_event: impl FnMut(&Event),
) -> Result<Report, Error> {
    let mut book = store.peer_book()?;
    let advertised: Vec<Service> = local_services(store)?.into_iter().map(|(s, _)| s).collect();
    let mut host = StoreHost::new(store, &mut book);
    let start = Instant::now();
    let config = link::Config {
        role: pass.role,
        mac: radio.mac(),
        token: random8(),
        services: advertised,
        channels: pass.channels.clone(),
        dwell: link::DWELL,
        linger: link::LINGER,
    };
    let mut link = Link::new(config, keys, start);
    let mut channel = link.channel();
    radio.set_channel(channel)?;
    let mut stop_at = None;
    loop {
        let now = Instant::now();
        link.poll(now);
        for (frame, rate) in link.take_output() {
            radio.send(&frame, rate)?;
        }
        for e in link.take_events() {
            if let Event::Abandoned { peer, .. } = &e {
                if host.declined.is_none() {
                    host.book.note_attempt(peer, now_secs())?;
                }
            }
            on_event(&e);
        }
        if link.is_done() {
            break;
        }
        if stop.load(Ordering::Relaxed) {
            let at = *stop_at.get_or_insert(now);
            if !link.is_engaged() || now - at > STOP_GRACE {
                break;
            }
        }
        if now - start >= pass.window && !link.is_engaged() {
            break;
        }
        if link.channel() != channel {
            channel = link.channel();
            radio.set_channel(channel)?;
        }
        if let Some(rx) = radio.recv(RECV_SLICE)? {
            link.on_frame(&rx.frame, &rx.info, Instant::now(), &mut host);
        }
    }
    host.finish(link.outcome())
}

#[cfg(test)]
mod tests;
