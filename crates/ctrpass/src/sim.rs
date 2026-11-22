//! A simulated console for exercising links in either role without a radio.

use std::time::{Duration, Instant};

use crate::cec::container::Container;
use crate::cec::object::{self, kind};
use crate::keys::{Key, Keys};
use crate::link::{Event, Host, Link};
use crate::streetpass::ie::{self, Kind};
use crate::streetpass::linkkey::{self, Token};
use crate::streetpass::service::Service;
use crate::wifi::ccmp;
use crate::wifi::frame::{Header, Mac, TYPE_DATA};
use crate::wifi::radiotap::RxInfo;
use crate::yy::frame::Segment;
use crate::yy::session::{self, Session};

pub const KEYS: &str = "slot0x2EKeyN=000102030405060708090a0b0c0d0e0f\n\
                        streetpassHmacKey=41414141414141414141414141414141\n";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Behaviour {
    /// Answers probes and hosts the link.
    Hosts,
    /// Probes and opens each object.
    Leads,
}

pub struct Console {
    pub mac: Mac,
    pub token: Token,
    keys: Keys,
    behaviour: Behaviour,
    services: Vec<Service>,
    objects: Vec<Vec<u8>>,
    /// Titles this console refuses in its verdict.
    pub refuse: Vec<u32>,
    /// How long after the first directed probe the link comes up.
    pub link_delay: Duration,
    /// Whether this console answers a host's message list at all.
    pub sends_messages: bool,
    peer: Option<(Mac, Token)>,
    key: Option<Key>,
    first_poll: Option<Instant>,
    linked: bool,
    session: Option<Session>,
    next_probe: Instant,
    output: Vec<Vec<u8>>,
    pn: u64,
    rx_pn: u64,
    seq: u16,
    verdict: Option<Vec<u8>>,
    answered_messages: bool,
}

impl Console {
    pub fn new(
        behaviour: Behaviour,
        messages: &[Container],
        services: Vec<Service>,
        now: Instant,
    ) -> Self {
        let mac = [0x96, 0x58, 0xcb, 0x11, 0x22, 0x33];
        let token = [0, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77];
        let mut objects = vec![
            object::build(kind::SESSION_OPEN, &[0; 0x28], 0),
            object::build(kind::TYPE_43, &[0; 16], 0),
            object::build(kind::SERVICES, &ie::link_records(&token, &services), 0xffff),
        ];
        objects.extend(object::outbox(messages, 0x1234).unwrap());
        Console {
            mac,
            token,
            keys: Keys::parse(KEYS).unwrap(),
            behaviour,
            services,
            objects,
            refuse: Vec::new(),
            link_delay: Duration::from_millis(500),
            sends_messages: true,
            peer: None,
            key: None,
            first_poll: None,
            linked: false,
            session: None,
            next_probe: now,
            output: Vec::new(),
            pn: 1,
            rx_pn: 0,
            seq: 100,
            verdict: None,
            answered_messages: false,
        }
    }

    pub fn take_output(&mut self) -> Vec<Vec<u8>> {
        std::mem::take(&mut self.output)
    }

    pub fn received(&self) -> &[Vec<u8>] {
        self.session.as_ref().map_or(&[], |s| s.received())
    }

    fn next_seq(&mut self) -> u16 {
        self.seq = (self.seq + 1) & 0xfff;
        self.seq
    }

    fn set_peer(&mut self, mac: Mac, token: Token) {
        let me = (&self.token, &self.mac);
        let them = (&token, &mac);
        self.key = Some(match self.behaviour {
            Behaviour::Hosts => linkkey::derive(&self.keys, me, them),
            Behaviour::Leads => linkkey::derive(&self.keys, them, me),
        });
        self.peer = Some((mac, token));
    }

    fn session_config() -> session::Config {
        session::Config {
            linger: Duration::from_secs(3),
            verdict_hold: Duration::ZERO,
            ..session::Config::default()
        }
    }

    pub fn hear(&mut self, frame: &[u8], now: Instant) {
        let Ok(h) = Header::parse(frame) else { return };
        if h.ftype() == TYPE_DATA && h.protected() {
            self.on_data(&h, frame, now);
            return;
        }
        let Ok(Some((h, kind))) = ie::classify(frame) else {
            return;
        };
        let to_me = h.addr1 == self.mac;
        match (self.behaviour, kind) {
            (Behaviour::Hosts, Kind::Probe(d)) if !self.linked => {
                let Some(token) = d.token else { return };
                if self.peer.is_none() {
                    self.set_peer(h.addr2, token);
                }
                let seq = self.next_seq();
                self.output.push(ie::offer(
                    self.mac,
                    h.addr2,
                    &self.token,
                    &self.services,
                    1,
                    seq,
                    0,
                ));
            }
            (Behaviour::Hosts, Kind::DirectedProbe) if to_me && self.peer.is_some() => {
                let first = *self.first_poll.get_or_insert(now);
                if now.saturating_duration_since(first) < self.link_delay {
                    return;
                }
                if !self.linked {
                    self.linked = true;
                    self.session = Some(Session::new(Vec::new(), Self::session_config(), now));
                }
                let seq = self.next_seq();
                self.output.push(ie::link_up(self.mac, h.addr2, 1, seq, 0));
            }
            (Behaviour::Leads, Kind::Offer(d)) if to_me && self.peer.is_none() => {
                let Some(token) = d.token else { return };
                self.set_peer(h.addr2, token);
                for _ in 0..2 {
                    let seq = self.next_seq();
                    self.output.push(ie::accept(self.mac, h.addr2, seq));
                }
                self.first_poll = Some(now);
            }
            (Behaviour::Leads, Kind::LinkUp) if to_me && self.peer.is_some() && !self.linked => {
                self.linked = true;
                let mut s = Session::new(self.objects.clone(), Self::session_config(), now);
                s.start(now);
                self.session = Some(s);
                self.flush();
            }
            _ => {}
        }
    }

    fn on_data(&mut self, h: &Header, frame: &[u8], now: Instant) {
        let (Some(key), Some(s)) = (self.key, self.session.as_mut()) else {
            return;
        };
        if h.addr1 != self.mac {
            return;
        }
        let Ok((pn, plain)) = ccmp::decrypt(&key, frame) else {
            return;
        };
        if pn <= self.rx_pn {
            return;
        }
        self.rx_pn = pn;
        let Some(seg) = Segment::parse(&plain) else {
            return;
        };
        if let Some(obj) = s.on_segment(&seg, now) {
            self.on_object(&obj);
        }
        self.flush();
    }

    fn verdict_on(&self, offer: &[u8], k: u32) -> Vec<u8> {
        object::set_codes(&object::retype(offer, k), |t| {
            Some(if self.refuse.contains(&t) {
                object::VERDICT_REFUSE
            } else {
                object::VERDICT_ADMIT
            })
        })
        .unwrap()
    }

    fn on_object(&mut self, obj: &[u8]) {
        let Some(k) = object::kind_of(obj) else {
            return;
        };
        match self.behaviour {
            Behaviour::Leads => {
                let s = self.session.as_mut().unwrap();
                if k == kind::OFFER_REPLY {
                    let i = s
                        .outbox()
                        .iter()
                        .position(|o| object::kind_of(o) == Some(kind::VERDICT))
                        .unwrap();
                    let v = self.verdict_on(obj, kind::VERDICT);
                    self.session.as_mut().unwrap().outbox_mut()[i] = v;
                } else if k == kind::VERDICT_REPLY {
                    if let Ok(v) = object::apply_verdict(s.outbox(), obj) {
                        *s.outbox_mut() = v.objects;
                    }
                }
            }
            Behaviour::Hosts => {
                let mine = self
                    .objects
                    .iter()
                    .find(|o| object::kind_of(o) == Some(k))
                    .cloned();
                let replies = match k {
                    kind::OFFER => {
                        self.verdict = Some(self.verdict_on(obj, kind::VERDICT_REPLY));
                        vec![object::retype(&mine.unwrap(), k + 1)]
                    }
                    kind::SESSION_OPEN | kind::SERVICES | kind::CONSOLE_ID => {
                        vec![object::retype(&mine.unwrap(), k + 1)]
                    }
                    kind::TYPE_43 => vec![mine.unwrap()],
                    kind::TYPE_23 => vec![object::build(k + 1, &2u32.to_le_bytes(), 0xffff)],
                    kind::VERDICT => {
                        if let Ok(v) = object::apply_verdict(&self.objects, obj) {
                            self.objects = v.objects;
                        }
                        vec![self.verdict.clone().unwrap()]
                    }
                    kind::MESSAGES if self.sends_messages && !self.answered_messages => {
                        self.answered_messages = true;
                        let i = self
                            .objects
                            .iter()
                            .position(|o| object::kind_of(o) == Some(kind::MESSAGES))
                            .unwrap();
                        self.objects[i..].to_vec()
                    }
                    _ => return,
                };
                self.session.as_mut().unwrap().outbox_mut().extend(replies);
            }
        }
    }

    fn flush(&mut self) {
        let (Some(key), Some((peer, _))) = (self.key, self.peer) else {
            return;
        };
        let Some(s) = self.session.as_mut() else {
            return;
        };
        let bssid = match self.behaviour {
            Behaviour::Hosts => self.mac,
            Behaviour::Leads => peer,
        };
        for seg in s.take_output() {
            self.seq = (self.seq + 1) & 0xfff;
            let mut h = Header::new(TYPE_DATA, 0, 0, peer, self.mac, bssid);
            h.set_sequence(self.seq);
            self.output
                .push(ccmp::encrypt(&key, &h, self.pn, &seg.to_bytes()).unwrap());
            self.pn += 1;
        }
    }

    pub fn poll(&mut self, now: Instant) {
        if let (Behaviour::Leads, Some((peer, _)), Some(at), false) =
            (self.behaviour, self.peer, self.first_poll, self.linked)
        {
            if now >= at {
                for _ in 0..3 {
                    let seq = self.next_seq();
                    self.output.push(ie::directed_probe(self.mac, peer, seq));
                }
                self.first_poll = Some(now + Duration::from_millis(300));
            }
        }
        if self.behaviour == Behaviour::Leads && self.peer.is_none() && now >= self.next_probe {
            let seq = self.next_seq();
            self.output
                .push(ie::probe(self.mac, &self.token, &self.services, seq));
            self.next_probe = now + Duration::from_millis(250);
        }
        if let Some(s) = self.session.as_mut() {
            s.poll(now);
        }
        self.flush();
    }
}

struct Air(u64, u32);

impl Air {
    /// Drops roughly one frame in `self.1`, or none when it is zero.
    fn drop(&mut self) -> bool {
        if self.1 == 0 {
            return false;
        }
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0 % self.1 as u64 == 0
    }
}

/// Runs a link against a console in simulated time until the link finishes or 30 seconds pass.
pub fn run(
    link: &mut Link,
    console: &mut Console,
    host: &mut dyn Host,
    loss: u32,
    seed: u64,
) -> Vec<Event> {
    let t0 = Instant::now();
    let mut now = t0;
    let mut air = Air(
        0x9e37_79b9_7f4a_7c15 ^ seed.wrapping_mul(0x2545_f491_4f6c_dd1d),
        loss,
    );
    let mut events = Vec::new();
    while !link.is_done() && now - t0 < Duration::from_secs(30) {
        link.poll(now);
        console.poll(now);
        for (f, _) in link.take_output() {
            if !air.drop() {
                console.hear(&f, now);
            }
        }
        for f in console.take_output() {
            if !air.drop() {
                link.on_frame(&f, &RxInfo::default(), now, host);
            }
        }
        events.extend(link.take_events());
        now += Duration::from_millis(5);
    }
    events
}
