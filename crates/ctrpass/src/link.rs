//! One StreetPass pass: finding a console, bringing up the encrypted link and trading objects over it.

use std::collections::{BTreeMap, HashMap};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::Error;
use crate::cec::container::{Container, Timestamp};
use crate::cec::object::{self, Entry, kind};
use crate::keys::{Key, Keys};
use crate::radio::{RATE_1M, RATE_11M};
use crate::streetpass::ie::{self, Discovery, Kind};
use crate::streetpass::linkkey::{self, Token};
use crate::streetpass::service::Service;
use crate::wifi::ccmp;
use crate::wifi::channel_of;
use crate::wifi::frame::{Header, Mac, TYPE_DATA};
use crate::wifi::radiotap::RxInfo;
use crate::yy::frame::Segment;
use crate::yy::session::{self, Repeat, Session};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    /// Probes, lets the console host the link, and opens each object.
    Lead,
    /// Answers a console's probe, hosts the link, and answers each object the console opens.
    Wait,
}

const PROBE_INTERVAL: Duration = Duration::from_millis(250);
/// Shortest gap between two directed probes sent to consoles heard while idle.
const REACT_GAP: Duration = Duration::from_millis(500);
const POLL_DELAY: Duration = Duration::from_millis(144);
const FIRST_BURST: u32 = 6;
const LATER_BURST: u32 = 2;
const POLL_INTERVAL: Duration = Duration::from_millis(300);
/// How long to keep asking a console to announce its link.
const LINK_PATIENCE: Duration = Duration::from_secs(3);
const ATTEMPT_TIMEOUT: Duration = Duration::from_secs(6);
const ACCEPT_PATIENCE: Duration = Duration::from_secs(2);
/// Repeated probes from an engaged console that are answered again.
const MAX_ANSWERS: u32 = 4;
/// Consoles send directed probes in bursts, and one answer per burst is enough.
const LINK_REPLY_GAP: Duration = Duration::from_millis(50);
/// How long a session stays open after the console sent its message list, for its messages to follow.
pub const LINGER: Duration = Duration::from_secs(2);
pub const DWELL: Duration = Duration::from_secs(6);
const DATA_DURATION: u16 = 0x2c;
const ACCEPTS: usize = 2;

/// The capability payload both sides send unchanged.
const CAPABILITIES: [u8; 16] = [
    0x1b, 0x00, 0x10, 0x00, 0x1a, 0x80, 0x0f, 0x00, 0xff, 0xff, 0xff, 0xff, 0x84, 0x54, 0x12, 0x00,
];

/// What to trade with one console.
#[derive(Clone, Debug, Default)]
pub struct Plan {
    pub services: Vec<Service>,
    pub messages: Vec<Container>,
}

/// Decides what to trade, so the link itself knows nothing about where messages are kept.
pub trait Host {
    /// Chooses what to trade with a console that engaged, or `None` to leave it alone.
    fn choose(&mut self, peer: Mac, services: &[Service]) -> Option<Plan>;
    /// The verdict code for each title of a console's offer.
    fn admit(&mut self, offer: &[Entry]) -> BTreeMap<u32, u16>;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    Engaged { peer: Mac },
    Declined { peer: Mac },
    LinkUp { peer: Mac },
    Abandoned { peer: Mac, reason: &'static str },
    Session(session::Note),
    Finished { peer: Mac },
}

/// What a finished or interrupted exchange moved.
#[derive(Clone, Debug)]
pub struct Outcome {
    pub peer: Mac,
    /// Objects the console acknowledged in full, as sent.
    pub delivered: Vec<Vec<u8>>,
    pub received: Vec<Vec<u8>>,
    /// Whether the console sent its message list, without which it keeps nothing.
    pub peer_sent_messages: bool,
    /// The console's offer with the local verdict codes, or `None` if the verdict went out before the offer arrived.
    pub admitted: Option<Vec<Entry>>,
    /// Local titles the console refused.
    pub refused: Vec<u32>,
}

#[derive(Clone, Debug)]
pub struct Config {
    pub role: Role,
    pub mac: Mac,
    pub token: Token,
    /// Services advertised in the broadcast probe, before a console narrows them.
    pub services: Vec<Service>,
    pub channels: Vec<u8>,
    pub dwell: Duration,
    pub linger: Duration,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Stage {
    Engaged,
    Accepted,
    Linked,
    Done,
}

struct Attempt {
    peer: Mac,
    key: Key,
    stage: Stage,
    since: Instant,
    objects: Vec<Vec<u8>>,
    /// Objects kept out of the session until the console's answer they depend on arrives.
    held: Vec<Vec<u8>>,
    services: Vec<Service>,
    session: Option<Session>,
    next_poll: Option<Instant>,
    polls: u32,
    give_up: Option<Instant>,
    answers: u32,
    last_link_reply: Option<Instant>,
    verdict: Option<Vec<u8>>,
    answered_messages: bool,
    admitted: Option<Vec<Entry>>,
    refused: Vec<u32>,
    rx_pn: u64,
}

pub struct Link {
    config: Config,
    keys: Keys,
    start: Instant,
    channel_index: usize,
    channel_locked: bool,
    next_hop: Instant,
    seq: u16,
    tx_pn: HashMap<Key, u64>,
    next_probe: Instant,
    last_react: Option<Instant>,
    declined: Vec<Mac>,
    attempt: Option<Attempt>,
    output: Vec<(Vec<u8>, u8)>,
    events: Vec<Event>,
}

fn random<const N: usize>() -> [u8; N] {
    let mut b = [0u8; N];
    getrandom::fill(&mut b).expect("the system random source failed");
    b
}

fn index_of(objects: &[Vec<u8>], k: u32) -> Option<usize> {
    objects.iter().position(|o| object::kind_of(o) == Some(k))
}

fn session_open(mac: Mac) -> Vec<u8> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let mut p = Vec::with_capacity(0x28);
    p.extend_from_slice(&random::<8>());
    p.extend_from_slice(&[0; 8]);
    p.extend_from_slice(&random::<4>());
    p.extend_from_slice(&mac);
    p.extend_from_slice(&[0; 2]);
    p.extend_from_slice(
        &Timestamp::from_unix(now.as_secs(), now.subsec_millis() as u16).to_bytes(),
    );
    object::build(kind::SESSION_OPEN, &p, 0)
}

/// The verdict on a console's offer: the local offer reordered when both name the same titles, else the console's own list.
fn lead_verdict(ours: &[u8], theirs: &[u8], codes: &BTreeMap<u32, u16>) -> Option<Vec<u8>> {
    let their_entries = object::parse_list(object::payload(theirs)).ok()?;
    let our_entries = object::parse_list(object::payload(ours)).ok()?;
    let mut a: Vec<u32> = their_entries.iter().map(|e| e.title).collect();
    let mut b: Vec<u32> = our_entries.iter().map(|e| e.title).collect();
    a.sort_unstable();
    b.sort_unstable();
    let base = if a == b {
        let ordered: Vec<Entry> = their_entries
            .iter()
            .filter_map(|t| our_entries.iter().find(|e| e.title == t.title).copied())
            .collect();
        object::build(kind::VERDICT, &object::build_list(&ordered).ok()?, 0xffff)
    } else {
        object::set_codes(&object::retype(theirs, kind::VERDICT), |_| {
            Some(object::VERDICT_ADMIT)
        })
        .ok()?
    };
    object::set_codes(&base, |t| codes.get(&t).copied()).ok()
}

fn with_codes(offer: &[u8], codes: &BTreeMap<u32, u16>) -> Option<Vec<Entry>> {
    let mut entries = object::parse_list(object::payload(offer)).ok()?;
    for e in &mut entries {
        e.code = codes
            .get(&e.title)
            .copied()
            .unwrap_or(object::VERDICT_ADMIT);
    }
    Some(entries)
}

impl Link {
    pub fn new(config: Config, keys: &Keys, now: Instant) -> Self {
        Link {
            next_hop: now + config.dwell,
            config,
            keys: keys.clone(),
            start: now,
            channel_index: 0,
            channel_locked: false,
            seq: u16::from_le_bytes(random::<2>()) & 0xfff,
            tx_pn: HashMap::new(),
            next_probe: now,
            last_react: None,
            declined: Vec::new(),
            attempt: None,
            output: Vec::new(),
            events: Vec::new(),
        }
    }

    pub fn role(&self) -> Role {
        self.config.role
    }

    /// The channel the radio should be on.
    pub fn channel(&self) -> u8 {
        self.config.channels[self.channel_index]
    }

    /// Frames to send with their rate, oldest first.
    pub fn take_output(&mut self) -> Vec<(Vec<u8>, u8)> {
        std::mem::take(&mut self.output)
    }

    pub fn take_events(&mut self) -> Vec<Event> {
        std::mem::take(&mut self.events)
    }

    /// Whether an exchange finished, after which the pass is over.
    pub fn is_done(&self) -> bool {
        self.attempt
            .as_ref()
            .is_some_and(|a| a.stage == Stage::Done)
    }

    /// Whether a console is engaged right now.
    pub fn is_engaged(&self) -> bool {
        self.attempt.is_some()
    }

    /// What the current exchange moved so far, if it got as far as a link.
    pub fn outcome(&self) -> Option<Outcome> {
        let a = self.attempt.as_ref()?;
        let s = a.session.as_ref()?;
        Some(Outcome {
            peer: a.peer,
            delivered: s.delivered().to_vec(),
            received: s.received().to_vec(),
            peer_sent_messages: s.peer_sent_messages(),
            admitted: a.admitted.clone(),
            refused: a.refused.clone(),
        })
    }

    fn next_seq(&mut self) -> u16 {
        self.seq = (self.seq + 1) & 0xfff;
        self.seq
    }

    fn tsf(&self, now: Instant) -> u64 {
        now.saturating_duration_since(self.start).as_micros() as u64
    }

    fn send(&mut self, frame: Vec<u8>, rate: u8) {
        self.output.push((frame, rate));
    }

    fn abandon(&mut self, reason: &'static str) {
        if let Some(a) = self.attempt.take() {
            self.events.push(Event::Abandoned {
                peer: a.peer,
                reason,
            });
        }
    }

    fn objects_for(&self, plan: &Plan) -> Result<Vec<Vec<u8>>, Error> {
        let mut out = vec![
            session_open(self.config.mac),
            object::build(kind::TYPE_43, &CAPABILITIES, 0),
            object::build(
                kind::SERVICES,
                &ie::link_records(&self.config.token, &plan.services),
                0xffff,
            ),
        ];
        out.extend(object::outbox(
            &plan.messages,
            u64::from_le_bytes(random::<8>()),
        )?);
        Ok(out)
    }

    fn engage(&mut self, peer: Mac, d: &Discovery, now: Instant, host: &mut dyn Host) -> bool {
        let Some(peer_token) = d.token else {
            return false;
        };
        self.attempt = None;
        let objects = host
            .choose(peer, &d.services)
            .and_then(|plan| Some((self.objects_for(&plan).ok()?, plan.services)));
        let Some((objects, services)) = objects else {
            self.declined.push(peer);
            self.events.push(Event::Declined { peer });
            return false;
        };
        let me = (&self.config.token, &self.config.mac);
        let them = (&peer_token, &peer);
        let key = match self.config.role {
            Role::Lead => linkkey::derive(&self.keys, them, me),
            Role::Wait => linkkey::derive(&self.keys, me, them),
        };
        self.attempt = Some(Attempt {
            peer,
            key,
            stage: Stage::Engaged,
            since: now,
            objects,
            held: Vec::new(),
            services,
            session: None,
            next_poll: None,
            polls: 0,
            give_up: None,
            answers: 0,
            last_link_reply: None,
            verdict: None,
            answered_messages: false,
            admitted: None,
            refused: Vec::new(),
            rx_pn: 0,
        });
        self.events.push(Event::Engaged { peer });
        true
    }

    /// Handles one received frame.
    pub fn on_frame(&mut self, frame: &[u8], info: &RxInfo, now: Instant, host: &mut dyn Host) {
        let Ok(h) = Header::parse(frame) else { return };
        if h.addr2 == self.config.mac {
            return;
        }
        if h.ftype() == TYPE_DATA && h.protected() {
            self.on_data(&h, frame, now, host);
            return;
        }
        let Ok(Some((h, kind))) = ie::classify(frame) else {
            return;
        };
        if matches!(kind, Kind::Probe(_) | Kind::Offer(_)) && !self.channel_locked {
            self.channel_locked = true;
            if let Some(c) = info.frequency.and_then(channel_of) {
                if let Some(i) = self.config.channels.iter().position(|&x| x == c) {
                    self.channel_index = i;
                }
            }
        }
        let to_me = h.addr1 == self.config.mac;
        let from_peer = self.attempt.as_ref().is_some_and(|a| a.peer == h.addr2);
        match (self.config.role, kind) {
            (Role::Lead, Kind::Offer(d)) if to_me => self.on_offer(h.addr2, &d, now, host),
            (Role::Lead, Kind::LinkUp) if to_me && from_peer => self.on_link_up(now),
            (Role::Lead, Kind::Probe(_)) if self.attempt.is_none() => self.react(h.addr2, now),
            (Role::Wait, Kind::Probe(d)) => self.on_probe(h.addr2, &d, now, host),
            (Role::Wait, Kind::Accept) if to_me && from_peer => {
                let a = self.attempt.as_mut().unwrap();
                a.stage = a.stage.max(Stage::Accepted);
            }
            (Role::Wait, Kind::DirectedProbe) if to_me && from_peer => self.on_directed_probe(now),
            _ => {}
        }
    }

    fn react(&mut self, console: Mac, now: Instant) {
        if self
            .last_react
            .is_some_and(|t| now.saturating_duration_since(t) < REACT_GAP)
        {
            return;
        }
        self.last_react = Some(now);
        let seq = self.next_seq();
        self.send(ie::directed_probe(self.config.mac, console, seq), RATE_1M);
        self.send_probe();
    }

    fn send_probe(&mut self) {
        let seq = self.next_seq();
        let probe = ie::probe(
            self.config.mac,
            &self.config.token,
            &self.config.services,
            seq,
        );
        self.send(probe, RATE_1M);
    }

    fn on_offer(&mut self, peer: Mac, d: &Discovery, now: Instant, host: &mut dyn Host) {
        if self
            .attempt
            .as_ref()
            .is_some_and(|a| a.stage >= Stage::Linked)
            || self.declined.contains(&peer)
        {
            return;
        }
        if !self.engage(peer, d, now, host) {
            return;
        }
        for _ in 0..ACCEPTS {
            let seq = self.next_seq();
            self.send(ie::accept(self.config.mac, peer, seq), RATE_11M);
        }
        let a = self.attempt.as_mut().unwrap();
        a.stage = Stage::Accepted;
        a.next_poll = Some(now + POLL_DELAY);
    }

    fn on_link_up(&mut self, now: Instant) {
        let a = self.attempt.as_mut().unwrap();
        if a.stage != Stage::Accepted {
            return;
        }
        a.stage = Stage::Linked;
        a.next_poll = None;
        a.give_up = None;
        a.since = now;
        let repeat = index_of(&a.objects, kind::MESSAGES).map(|after| Repeat {
            after,
            object: object::build(kind::SCANNING, &[0; 4], 0xffff),
            max: 1,
        });
        let config = session::Config {
            linger: ATTEMPT_TIMEOUT,
            repeat,
            ..session::Config::default()
        };
        let mut outbox = a.objects.clone();
        a.held = index_of(&outbox, kind::VERDICT)
            .map(|i| outbox.split_off(i))
            .unwrap_or_default();
        let mut s = Session::new(outbox, config, now);
        s.start(now);
        a.session = Some(s);
        let peer = a.peer;
        self.events.push(Event::LinkUp { peer });
        self.flush();
    }

    fn on_probe(&mut self, peer: Mac, d: &Discovery, now: Instant, host: &mut dyn Host) {
        if d.token.is_none() || self.declined.contains(&peer) {
            return;
        }
        match &self.attempt {
            Some(a) if a.peer == peer && a.stage == Stage::Engaged && a.answers < MAX_ANSWERS => {}
            Some(_) => return,
            None => {
                if !self.engage(peer, d, now, host) {
                    return;
                }
            }
        }
        let channel = d.channel.unwrap_or_else(|| self.channel());
        let seq = self.next_seq();
        let tsf = self.tsf(now);
        let a = self.attempt.as_mut().unwrap();
        a.answers += 1;
        let offer = ie::offer(
            self.config.mac,
            peer,
            &self.config.token,
            &a.services,
            channel,
            seq,
            tsf,
        );
        self.send(offer, RATE_1M);
    }

    fn on_directed_probe(&mut self, now: Instant) {
        let channel = self.channel();
        let seq = self.next_seq();
        let tsf = self.tsf(now);
        let a = self.attempt.as_mut().unwrap();
        if a.last_link_reply
            .is_some_and(|t| now.saturating_duration_since(t) < LINK_REPLY_GAP)
        {
            return;
        }
        a.last_link_reply = Some(now);
        let peer = a.peer;
        let first = a.stage < Stage::Linked;
        if first {
            a.stage = Stage::Linked;
            a.since = now;
            let config = session::Config {
                linger: ATTEMPT_TIMEOUT,
                verdict_hold: Duration::ZERO,
                ..session::Config::default()
            };
            a.session = Some(Session::new(Vec::new(), config, now));
        }
        self.send(
            ie::link_up(self.config.mac, peer, channel, seq, tsf),
            RATE_1M,
        );
        if first {
            self.events.push(Event::LinkUp { peer });
        }
    }

    fn on_data(&mut self, h: &Header, frame: &[u8], now: Instant, host: &mut dyn Host) {
        let Some(a) = self.attempt.as_mut() else {
            return;
        };
        if h.addr2 != a.peer || h.addr1 != self.config.mac || a.session.is_none() {
            return;
        }
        let Ok((pn, plain)) = ccmp::decrypt(&a.key, frame) else {
            return;
        };
        if pn <= a.rx_pn {
            return;
        }
        a.rx_pn = pn;
        let Some(seg) = Segment::parse(&plain) else {
            return;
        };
        a.since = now;
        let obj = a.session.as_mut().unwrap().on_segment(&seg, now);
        if let Some(obj) = obj {
            match self.config.role {
                Role::Lead => self.lead_object(&obj, host),
                Role::Wait => self.answer_object(&obj, host),
            }
        }
        self.flush();
    }

    fn lead_object(&mut self, obj: &[u8], host: &mut dyn Host) {
        let a = self.attempt.as_mut().unwrap();
        let s = a.session.as_mut().unwrap();
        match object::kind_of(obj) {
            Some(kind::OFFER_REPLY)
                if a.held.first().and_then(|o| object::kind_of(o)) == Some(kind::VERDICT) =>
            {
                let codes = with_codes(obj, &BTreeMap::new())
                    .map(|entries| host.admit(&entries))
                    .unwrap_or_default();
                let ours = a.held.remove(0);
                let verdict = lead_verdict(&ours, obj, &codes);
                if verdict.is_some() {
                    a.admitted = with_codes(obj, &codes);
                }
                s.outbox_mut().push(verdict.unwrap_or(ours));
            }
            Some(kind::VERDICT_REPLY) if !a.held.is_empty() => {
                let mut all = s.outbox().to_vec();
                all.append(&mut a.held);
                match object::apply_verdict(&all, obj) {
                    Ok(v) => {
                        *s.outbox_mut() = v.objects;
                        a.refused = v.refused;
                    }
                    Err(_) => *s.outbox_mut() = all,
                }
            }
            _ => {}
        }
    }

    fn answer_object(&mut self, obj: &[u8], host: &mut dyn Host) {
        let a = self.attempt.as_mut().unwrap();
        let Some(k) = object::kind_of(obj) else {
            return;
        };
        let mine = index_of(&a.objects, k).map(|i| a.objects[i].clone());
        let replies: Vec<Vec<u8>> = match k {
            kind::SESSION_OPEN | kind::SERVICES | kind::CONSOLE_ID | kind::OFFER => {
                let Some(mine) = mine else { return };
                if k == kind::OFFER {
                    let entries = with_codes(obj, &BTreeMap::new()).unwrap_or_default();
                    let codes = host.admit(&entries);
                    a.verdict = object::set_codes(&object::retype(obj, kind::VERDICT_REPLY), |t| {
                        Some(codes.get(&t).copied().unwrap_or(object::VERDICT_ADMIT))
                    })
                    .ok();
                    a.admitted = with_codes(obj, &codes);
                }
                vec![object::retype(&mine, k + 1)]
            }
            kind::TYPE_43 => mine.into_iter().collect(),
            kind::TYPE_23 => vec![object::build(k + 1, &2u32.to_le_bytes(), 0xffff)],
            kind::VERDICT => {
                if let Ok(v) = object::apply_verdict(&a.objects, obj) {
                    a.objects = v.objects;
                    a.refused = v.refused;
                }
                vec![
                    a.verdict
                        .clone()
                        .unwrap_or_else(|| object::empty_list(kind::VERDICT_REPLY)),
                ]
            }
            kind::MESSAGES if !a.answered_messages => {
                a.answered_messages = true;
                match index_of(&a.objects, kind::MESSAGES) {
                    Some(i) => a.objects[i..].to_vec(),
                    None => return,
                }
            }
            _ => return,
        };
        a.session.as_mut().unwrap().outbox_mut().extend(replies);
    }

    /// Encrypts whatever the session queued and checks whether it ended.
    fn flush(&mut self) {
        let Some(a) = self.attempt.as_mut() else {
            return;
        };
        let Some(s) = a.session.as_mut() else { return };
        let segments = s.take_output();
        let notes = s.take_notes();
        let ended = s.is_done();
        let complete = s.peer_sent_messages();
        if complete {
            s.set_linger(self.config.linger);
        }
        let (peer, key) = (a.peer, a.key);
        let bssid = match self.config.role {
            Role::Lead => peer,
            Role::Wait => self.config.mac,
        };
        for seg in segments {
            let pn = self.tx_pn.entry(key).or_insert(1);
            let this = *pn;
            *pn += 1;
            let seq = self.next_seq();
            let mut h = Header::new(TYPE_DATA, 0, 0, peer, self.config.mac, bssid);
            h.duration = DATA_DURATION;
            h.set_sequence(seq);
            if let Ok(f) = ccmp::encrypt(&key, &h, this, &seg.to_bytes()) {
                self.send(f, RATE_11M);
            }
        }
        self.events.extend(notes.into_iter().map(Event::Session));
        if ended {
            if complete {
                self.attempt.as_mut().unwrap().stage = Stage::Done;
                self.events.push(Event::Finished { peer });
            } else {
                self.abandon("the console stored nothing");
            }
        }
    }

    /// Drives timers: probing, asking for the link, timeouts and the session.
    pub fn poll(&mut self, now: Instant) {
        if self.is_done() {
            return;
        }
        if self.attempt.is_none() {
            if self.config.role == Role::Lead && now >= self.next_probe {
                self.send_probe();
                self.next_probe = now + PROBE_INTERVAL;
            }
            if !self.channel_locked && self.config.channels.len() > 1 && now >= self.next_hop {
                self.channel_index = (self.channel_index + 1) % self.config.channels.len();
                self.next_hop = now + self.config.dwell;
            }
            return;
        }
        let a = self.attempt.as_mut().unwrap();
        if a.next_poll.is_some_and(|t| now >= t) {
            let burst = if a.give_up.is_none() {
                a.give_up = Some(now + LINK_PATIENCE);
                FIRST_BURST
            } else {
                LATER_BURST
            };
            a.polls += 1;
            let next = now + POLL_INTERVAL;
            a.next_poll = a.give_up.filter(|&g| next < g).map(|_| next);
            let peer = a.peer;
            for _ in 0..burst {
                let seq = self.next_seq();
                self.send(ie::directed_probe(self.config.mac, peer, seq), RATE_1M);
            }
        }
        let a = self.attempt.as_mut().unwrap();
        if a.stage == Stage::Accepted && a.give_up.is_some_and(|g| now >= g) {
            self.abandon("the console did not announce its link");
            return;
        }
        if self.config.role == Role::Wait
            && a.stage == Stage::Engaged
            && now.saturating_duration_since(a.since) > ACCEPT_PATIENCE
        {
            self.abandon("the console did not take the answer");
            return;
        }
        // A session ends itself when the console falls silent, so this only catches one stuck mid-object.
        let limit = if a.session.is_some() {
            ATTEMPT_TIMEOUT * 2
        } else {
            ATTEMPT_TIMEOUT
        };
        if now.saturating_duration_since(a.since) > limit {
            self.abandon("timed out");
            return;
        }
        if let Some(s) = a.session.as_mut() {
            s.poll(now);
        }
        self.flush();
    }
}

#[cfg(test)]
mod tests;
