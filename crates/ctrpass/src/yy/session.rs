//! The object exchange, as a state machine fed with segments and the time that queues segments to send.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use super::frame::{ACK, END, FIN, SYN, Segment, WINDOW, segments};
use crate::cec::envelope;
use crate::cec::object::{self, kind};

/// Repeated ACKs at the same offset that trigger a retransmission.
const DUP_ACKS: u32 = 2;
const RETRANSMIT_AFTER: Duration = Duration::from_millis(750);
/// Attempts without progress before giving up on the peer.
const MAX_RESENDS: u32 = 16;
pub const MAX_OBJECT: usize = 1 << 20;
pub const MAX_RECEIVED_OBJECTS: usize = 64;
pub const MAX_RECEIVED_BYTES: usize = 8 << 20;
/// Pause after the peer's verdict, so its next poll sees the verdict delivered before the message list arrives.
pub const VERDICT_HOLD: Duration = Duration::from_millis(60);
const IDLE_TIMEOUT: Duration = Duration::from_millis(400);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Role {
    Idle,
    Send,
    Receive,
}

/// Something worth reporting that does not change what the caller has to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Note {
    Retransmit { object: usize, from: u32 },
    GaveUp { object: usize, acked: u32 },
    Refused { object: usize, reason: String },
    RepeatedObject { object: usize },
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    pub sent_objects: u32,
    pub received_objects: u32,
    pub tx_frames: u32,
    pub rx_frames: u32,
    pub resends: u32,
    pub syn_resends: u32,
    pub duplicate_tails: u32,
    pub gaps: u32,
    pub refused: u32,
    pub window_probes: u32,
    pub repeats: u32,
}

/// Sends one object again after the message list when the peer answers it with nothing.
#[derive(Clone, Debug)]
pub struct Repeat {
    /// Index in the outbox of the message list.
    pub after: usize,
    pub object: Vec<u8>,
    pub max: u32,
}

#[derive(Clone, Debug)]
pub struct Config {
    /// How long to stay open with nothing left to send, so a peer can keep sending.
    pub linger: Duration,
    pub verdict_hold: Duration,
    /// How long to wait for the peer to open a connection before sending again.
    pub idle_timeout: Duration,
    pub repeat: Option<Repeat>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            linger: Duration::ZERO,
            verdict_hold: VERDICT_HOLD,
            idle_timeout: IDLE_TIMEOUT,
            repeat: None,
        }
    }
}

pub struct Session {
    config: Config,
    now: Instant,
    outbox: Vec<Vec<u8>>,
    next: usize,
    output: VecDeque<Segment>,
    notes: Vec<Note>,
    pub stats: Stats,
    done: bool,
    failed: bool,
    role: Role,

    tx_obj: Vec<u8>,
    tx_index: Option<usize>,
    segs: VecDeque<(u8, u32, Vec<u8>)>,
    tx_seq: u32,
    /// Highest offset sent for the current object, which retransmissions do not lower.
    tx_high: u32,
    tx_acked: u32,
    peer_window: u16,
    dup_acks: u32,
    /// Set after a duplicate-ACK retransmission until the acknowledged offset moves.
    recovering: bool,
    resends: u32,
    syn_at: Option<Instant>,
    syn_resends: u32,
    window_probes: u32,
    last_progress: Instant,
    idle_since: Option<Instant>,
    send_at: Option<Instant>,
    start_pending: bool,
    last_activity: Instant,

    rx_buf: Vec<u8>,
    rx_expect: Option<usize>,
    rx_gap: bool,
    rx_refused: bool,
    rx_done_len: u32,
    rx_total: usize,
    received: Vec<Vec<u8>>,
    delivered: Vec<Vec<u8>>,
    peer_fin: bool,
    repeats_done: u32,
    repeat_answered: bool,
}

impl Session {
    pub fn new(outbox: Vec<Vec<u8>>, config: Config, now: Instant) -> Self {
        Session {
            config,
            now,
            outbox,
            next: 0,
            output: VecDeque::new(),
            notes: Vec::new(),
            stats: Stats::default(),
            done: false,
            failed: false,
            role: Role::Idle,
            tx_obj: Vec::new(),
            tx_index: None,
            segs: VecDeque::new(),
            tx_seq: 0,
            tx_high: 0,
            tx_acked: 0,
            peer_window: WINDOW,
            dup_acks: 0,
            recovering: false,
            resends: 0,
            syn_at: None,
            syn_resends: 0,
            window_probes: 0,
            last_progress: now,
            idle_since: None,
            send_at: None,
            start_pending: false,
            last_activity: now,
            rx_buf: Vec::new(),
            rx_expect: None,
            rx_gap: false,
            rx_refused: false,
            rx_done_len: 0,
            rx_total: 0,
            received: Vec::new(),
            delivered: Vec::new(),
            peer_fin: false,
            repeats_done: 0,
            repeat_answered: false,
        }
    }

    pub fn is_done(&self) -> bool {
        self.done
    }

    /// Whether the session ended because the peer stopped answering.
    pub fn failed(&self) -> bool {
        self.failed
    }

    fn give_up(&mut self, acked: u32) {
        self.notes.push(Note::GaveUp {
            object: self.next,
            acked,
        });
        self.failed = true;
        self.done = true;
    }

    /// Segments to send, oldest first.
    pub fn take_output(&mut self) -> Vec<Segment> {
        self.output.drain(..).collect()
    }

    pub fn take_notes(&mut self) -> Vec<Note> {
        std::mem::take(&mut self.notes)
    }

    pub fn received(&self) -> &[Vec<u8>] {
        &self.received
    }

    /// Objects the peer acknowledged in full, as they were sent.
    pub fn delivered(&self) -> &[Vec<u8>] {
        &self.delivered
    }

    pub fn outbox(&self) -> &[Vec<u8>] {
        &self.outbox
    }

    /// Objects from `next_index()` on have not been sent yet and may be changed.
    pub fn outbox_mut(&mut self) -> &mut Vec<Vec<u8>> {
        &mut self.outbox
    }

    pub fn next_index(&self) -> usize {
        self.next
    }

    pub fn set_linger(&mut self, linger: Duration) {
        self.config.linger = linger;
    }

    /// Whether the peer got as far as sending its message list, without which it keeps nothing it received.
    pub fn peer_sent_messages(&self) -> bool {
        self.peer_fin
            || self
                .received
                .iter()
                .any(|o| envelope::title(o).is_some() || object::kind_of(o) == Some(kind::MESSAGES))
    }

    fn emit(&mut self, flags: u8, seq: u32, ack: u32, payload: Vec<u8>) {
        self.stats.tx_frames += 1;
        self.last_activity = self.now;
        self.output
            .push_back(Segment::new(flags, seq, ack, payload));
    }

    /// Opens a connection for the next object in the outbox.
    pub fn start(&mut self, now: Instant) {
        self.now = now;
        self.start_send();
    }

    fn start_send(&mut self) {
        if self.next >= self.outbox.len() {
            if self.config.linger.is_zero() {
                self.done = true;
            }
            return;
        }
        if self.role == Role::Send && self.tx_index == Some(self.next) {
            return;
        }
        self.role = Role::Send;
        self.tx_index = Some(self.next);
        self.tx_obj = self.outbox[self.next].clone();
        self.segs = segments(&self.tx_obj).into();
        self.tx_seq = 0;
        self.tx_high = 0;
        self.tx_acked = 0;
        self.dup_acks = 0;
        self.recovering = false;
        self.resends = 0;
        self.syn_resends = 0;
        self.peer_window = WINDOW;
        self.window_probes = 0;
        self.last_progress = self.now;
        self.syn_at = Some(self.now);
        self.emit(SYN, 0, 0, Vec::new());
    }

    fn pump_send(&mut self) {
        let limit = self.tx_acked + self.peer_window.min(WINDOW) as u32;
        let mut sent = false;
        while let Some((flags, seq, chunk)) = self.segs.front().cloned() {
            let room = limit.saturating_sub(seq) as usize;
            if room == 0 && !chunk.is_empty() {
                break;
            }
            let (flags, chunk) = if chunk.len() > room {
                self.segs[0] = (flags, seq + room as u32, chunk[room..].to_vec());
                (ACK, chunk[..room].to_vec())
            } else {
                self.segs.pop_front();
                (flags, chunk)
            };
            let end = seq + chunk.len() as u32;
            self.emit(flags, seq, 0, chunk);
            self.tx_seq = self.tx_seq.max(end);
            self.tx_high = self.tx_high.max(end);
            sent = true;
            if flags & END != 0 || end >= limit {
                break;
            }
        }
        if sent {
            self.window_probes = 0;
        }
    }

    fn retransmit(&mut self) -> bool {
        if self.resends >= MAX_RESENDS {
            self.give_up(self.tx_acked);
            return false;
        }
        self.resends += 1;
        self.stats.resends += 1;
        self.dup_acks = 0;
        let acked = self.tx_acked;
        self.segs = segments(&self.tx_obj)
            .into_iter()
            .filter(|(_, seq, p)| seq + p.len() as u32 > acked)
            .map(|(f, seq, p)| {
                let skip = acked.saturating_sub(seq) as usize;
                (f, seq.max(acked), p[skip..].to_vec())
            })
            .collect();
        self.tx_seq = acked;
        self.last_progress = self.now;
        self.notes.push(Note::Retransmit {
            object: self.next,
            from: acked,
        });
        true
    }

    fn refuse(&mut self, reason: String) {
        self.stats.refused += 1;
        self.notes.push(Note::Refused {
            object: self.received.len(),
            reason,
        });
        self.rx_refused = true;
        self.rx_buf.clear();
        self.rx_expect = None;
    }

    fn object_complete(&self) -> bool {
        let n = self.rx_buf.len();
        match self.rx_expect {
            _ if n == 0 => false,
            Some(expect) => n >= expect,
            None => !self.rx_gap && n % WINDOW as usize != 0,
        }
    }

    /// Handles one segment and returns any object it completes; the reply waits for `poll` so the outbox can change first.
    pub fn on_segment(&mut self, y: &Segment, now: Instant) -> Option<Vec<u8>> {
        self.now = now;
        self.stats.rx_frames += 1;
        self.last_activity = now;
        let f = y.flags;
        if f & FIN != 0 {
            self.peer_fin = true;
        }

        if f & SYN != 0 && f & ACK == 0 {
            // The peer only takes its turn after receiving a whole object, so a lost final ACK is implied.
            if self.role == Role::Send
                && self.tx_high > 0
                && self.tx_high as usize == self.tx_obj.len()
            {
                self.finish_send();
            }
            self.send_at = None;
            self.start_pending = false;
            self.role = Role::Receive;
            self.rx_buf.clear();
            self.rx_expect = None;
            self.rx_gap = false;
            self.rx_refused = false;
            self.emit(SYN | ACK, 0, 0, Vec::new());
            return None;
        }

        if f & SYN != 0 {
            if self.role != Role::Send {
                return None;
            }
            self.syn_at = None;
            if self.tx_acked == 0 {
                self.peer_window = y.window;
            }
            self.emit(ACK, 0, 0, Vec::new());
            if self.tx_seq == 0 {
                self.pump_send();
            }
            return None;
        }

        let len = y.payload.len();
        // Data without a fresh SYN is the peer resending an object whose final ACK it missed.
        if len > 0
            && self.rx_buf.is_empty()
            && self.role != Role::Receive
            && (y.seq > 0 || !self.received.is_empty())
        {
            self.stats.duplicate_tails += 1;
            if f & END != 0 {
                self.emit(ACK, 0, self.rx_done_len, Vec::new());
            }
            return None;
        }
        if len > 0 {
            return self.on_data(y);
        }

        if f & ACK != 0 {
            self.on_ack(y);
        }
        None
    }

    fn on_data(&mut self, y: &Segment) -> Option<Vec<u8>> {
        if self.rx_refused {
            return None;
        }
        let have = self.rx_buf.len();
        if y.seq as usize == have {
            if have + y.payload.len() > MAX_OBJECT {
                self.refuse(format!("longer than {MAX_OBJECT} bytes"));
                return None;
            }
            self.rx_buf.extend_from_slice(&y.payload);
            if self.rx_expect.is_none() && self.rx_buf.len() >= object::HEADER_LEN {
                self.rx_expect = object::declared_len(&self.rx_buf)
                    .filter(|n| (object::HEADER_LEN..=MAX_OBJECT).contains(n));
                let known =
                    self.rx_buf[..2] == object::MAGIC || self.rx_buf[..2] == envelope::MAGIC;
                if self.rx_expect.is_none() && known {
                    self.refuse("declared length out of range".into());
                    return None;
                }
            }
        } else if (y.seq as usize) > have {
            self.rx_gap = true;
            self.stats.gaps += 1;
            self.emit(ACK, 0, have as u32, Vec::new());
        } else if y.flags & END == 0 {
            // Repeated data means an ACK went missing, so answer with the current offset.
            self.emit(ACK, 0, have as u32, Vec::new());
        }
        if y.flags & END == 0 {
            return None;
        }
        self.emit(ACK, 0, self.rx_buf.len() as u32, Vec::new());
        if !self.object_complete() {
            return None;
        }
        let mut obj = std::mem::take(&mut self.rx_buf);
        if let Some(n) = self.rx_expect {
            obj.truncate(n);
        }
        if self.received.len() >= MAX_RECEIVED_OBJECTS
            || self.rx_total + obj.len() > MAX_RECEIVED_BYTES
        {
            self.refuse("session already holds too much".into());
            return None;
        }
        self.rx_total += obj.len();
        self.rx_done_len = obj.len() as u32;
        self.stats.received_objects += 1;
        if self.repeats_done > 0 && !self.repeat_answered {
            self.repeat_answered = true;
        }
        self.rx_expect = None;
        self.rx_gap = false;
        self.role = Role::Idle;
        self.idle_since = None;
        if !self.config.verdict_hold.is_zero() && object::kind_of(&obj) == Some(kind::VERDICT_REPLY)
        {
            self.send_at = Some(self.now + self.config.verdict_hold);
        } else {
            self.start_pending = true;
        }
        self.received.push(obj.clone());
        Some(obj)
    }

    fn on_ack(&mut self, y: &Segment) {
        if self.role != Role::Send {
            return;
        }
        if y.ack as usize > self.tx_obj.len() || (self.syn_at.is_some() && y.ack != 0) {
            return;
        }
        let (before, window_before) = (self.tx_acked, self.peer_window);
        if y.ack >= self.tx_acked {
            self.peer_window = y.window;
        }
        self.tx_acked = self.tx_acked.max(y.ack);
        if self.tx_acked > before {
            self.dup_acks = 0;
            self.resends = 0;
            self.recovering = false;
            self.last_progress = self.now;
        } else if self.peer_window != window_before {
            self.last_progress = self.now;
        } else if self.tx_seq > self.tx_acked && self.tx_acked > 0 {
            self.dup_acks += 1;
            if self.dup_acks >= DUP_ACKS && self.segs.is_empty() && !self.recovering {
                self.recovering = self.retransmit();
            }
        }
        let whole = self.tx_acked as usize >= self.tx_obj.len();
        if self.tx_seq > 0 && (whole || (self.segs.is_empty() && self.tx_acked >= self.tx_seq)) {
            self.finish_send();
            return;
        }
        self.pump_send();
    }

    fn finish_send(&mut self) {
        self.delivered.push(std::mem::take(&mut self.tx_obj));
        self.next += 1;
        self.stats.sent_objects += 1;
        self.role = Role::Idle;
        self.tx_index = None;
        self.idle_since = Some(self.now);
    }

    /// Drives timers: retransmissions, deferred sends, lingering and taking the next turn.
    pub fn poll(&mut self, now: Instant) {
        self.now = now;
        if self.done {
            return;
        }
        if self.start_pending {
            self.start_pending = false;
            self.start_send();
            return;
        }
        if !self.config.linger.is_zero()
            && self.role == Role::Idle
            && self.send_at.is_none()
            && self.next >= self.outbox.len()
            && (self.peer_fin
                || now.saturating_duration_since(self.last_activity) > self.config.linger)
        {
            self.done = true;
            return;
        }
        if let Some(at) = self.send_at {
            if now >= at {
                self.send_at = None;
                self.start_send();
            }
            return;
        }
        if self.role == Role::Send {
            if let Some(at) = self.syn_at {
                if now.saturating_duration_since(at) > RETRANSMIT_AFTER {
                    if self.syn_resends < MAX_RESENDS {
                        self.syn_resends += 1;
                        self.stats.syn_resends += 1;
                        self.syn_at = Some(now);
                        self.emit(SYN, 0, 0, Vec::new());
                    } else {
                        self.give_up(0);
                    }
                }
                return;
            }
            let stalled = now.saturating_duration_since(self.last_progress) > RETRANSMIT_AFTER;
            if stalled && self.tx_seq > self.tx_acked {
                if self.retransmit() {
                    self.pump_send();
                }
            } else if stalled && !self.segs.is_empty() {
                self.probe_window();
            }
            return;
        }
        if self.role != Role::Idle {
            return;
        }
        let Some(idle) = self.idle_since else { return };
        if now.saturating_duration_since(idle) < self.config.idle_timeout {
            return;
        }
        self.idle_since = None;
        self.maybe_repeat();
        self.start_send();
    }

    fn probe_window(&mut self) {
        if self.window_probes >= MAX_RESENDS {
            self.give_up(self.tx_acked);
            return;
        }
        self.window_probes += 1;
        self.stats.window_probes += 1;
        self.last_progress = self.now;
        self.emit(ACK, self.tx_seq, 0, Vec::new());
    }

    fn maybe_repeat(&mut self) {
        let Some(r) = &self.config.repeat else { return };
        if self.repeats_done >= r.max || self.repeat_answered || self.next != r.after + 1 {
            return;
        }
        let obj = r.object.clone();
        self.repeats_done += 1;
        self.stats.repeats += 1;
        self.outbox.insert(self.next, obj);
        if let Some(r) = &mut self.config.repeat {
            r.after = self.next;
        }
        self.notes.push(Note::RepeatedObject { object: self.next });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cec::object::build;
    use crate::yy::frame::PSH;

    struct Lossy(u64, u32);

    impl Lossy {
        /// Drops roughly one frame in `self.1`.
        fn drop(&mut self) -> bool {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0 % self.1 as u64 == 0
        }
    }

    fn objects(tag: u8, sizes: &[usize]) -> Vec<Vec<u8>> {
        sizes
            .iter()
            .enumerate()
            .map(|(i, &n)| {
                let payload: Vec<u8> = (0..n).map(|j| (j as u8) ^ tag ^ i as u8).collect();
                build(0x20 + i as u32, &payload, 0xffff)
            })
            .collect()
    }

    fn dump(s: &Session) -> String {
        format!(
            "failed {} {:?} next {}/{} received {} rx {} of {:?} gap {} tx {}/{} acked segs {} syn {} resends {}/{} probes {} idle {} pending {}",
            s.failed,
            s.role,
            s.next,
            s.outbox.len(),
            s.received.len(),
            s.rx_buf.len(),
            s.rx_expect,
            s.rx_gap,
            s.tx_seq,
            s.tx_acked,
            s.segs.len(),
            s.syn_at.is_some(),
            s.resends,
            s.syn_resends,
            s.window_probes,
            s.idle_since.is_some(),
            s.start_pending,
        )
    }

    fn config() -> Config {
        Config {
            linger: Duration::from_secs(2),
            ..Config::default()
        }
    }

    /// Runs two sessions against each other until both finish, returning them.
    fn exchange(a: Vec<Vec<u8>>, b: Vec<Vec<u8>>, loss: u32, seed: u64) -> (Session, Session) {
        let t0 = Instant::now();
        let mut now = t0;
        let mut lead = Session::new(a, config(), now);
        let mut wait = Session::new(b, config(), now);
        let mut air = Lossy(
            0x9e37_79b9_7f4a_7c15 ^ seed.wrapping_mul(0x2545_f491_4f6c_dd1d),
            loss,
        );
        lead.start(now);
        while !(lead.is_done() && wait.is_done()) && !lead.failed() && !wait.failed() {
            assert!(
                now - t0 < Duration::from_secs(120),
                "exchange stalled at loss 1/{loss}, seed {seed}\nlead {}\nwait {}",
                dump(&lead),
                dump(&wait)
            );
            for s in lead.take_output() {
                if loss == 0 || !air.drop() {
                    wait.on_segment(&s, now);
                }
            }
            for s in wait.take_output() {
                if loss == 0 || !air.drop() {
                    lead.on_segment(&s, now);
                }
            }
            now += Duration::from_millis(5);
            lead.poll(now);
            wait.poll(now);
        }
        (lead, wait)
    }

    #[test]
    fn alternating_exchange() {
        let a = objects(1, &[8, 0x40, 40_000, 4]);
        let b = objects(2, &[8, 0x200, 30_000]);
        let (lead, wait) = exchange(a.clone(), b.clone(), 0, 0);
        assert_eq!(wait.received(), &a[..]);
        assert_eq!(lead.received(), &b[..]);
        assert_eq!(lead.delivered(), &a[..]);
        assert_eq!(lead.stats.resends, 0);
    }

    #[test]
    fn exchange_survives_loss() {
        let a = objects(3, &[8, 0x40, 40_000, 4, 15_000]);
        let b = objects(4, &[8, 0x200, 30_000]);
        for loss in [4, 5, 8] {
            for seed in 0..100 {
                let (lead, wait) = exchange(a.clone(), b.clone(), loss, seed);
                let why = format!(
                    "loss 1/{loss}, seed {seed}\nlead {}\nwait {}",
                    dump(&lead),
                    dump(&wait)
                );
                assert_eq!(wait.received().len(), a.len(), "{why}");
                assert_eq!(lead.received().len(), b.len(), "{why}");
                assert_eq!((wait.received(), lead.received()), (&a[..], &b[..]));
            }
        }
    }

    #[test]
    fn heavy_loss_ends_cleanly() {
        let a = objects(3, &[8, 0x40, 40_000, 4, 15_000]);
        let b = objects(4, &[8, 0x200, 30_000]);
        let mut failures = 0;
        for seed in 0..100 {
            let (lead, wait) = exchange(a.clone(), b.clone(), 3, seed);
            let kinds = |v: &[Vec<u8>]| {
                v.iter()
                    .map(|o| (object::kind_of(o), o.len()))
                    .collect::<Vec<_>>()
            };
            let why = format!(
                "seed {seed}\nlead {} got {:?}\nwait {} got {:?}",
                dump(&lead),
                kinds(lead.received()),
                dump(&wait),
                kinds(wait.received())
            );
            assert!(a.starts_with(wait.received()), "{why}");
            assert!(b.starts_with(lead.received()), "{why}");
            if lead.failed() || wait.failed() {
                failures += 1;
            } else {
                assert_eq!((wait.received(), lead.received()), (&a[..], &b[..]));
            }
        }
        assert!(failures <= 5, "{failures} of 100 failed");
    }

    #[test]
    fn receive_only_side() {
        let a = objects(5, &[8, 3000]);
        let (_, wait) = exchange(a.clone(), Vec::new(), 4, 1);
        assert_eq!(wait.received(), &a[..]);
    }

    #[test]
    fn refuses_impossible_length() {
        let now = Instant::now();
        let mut s = Session::new(Vec::new(), config(), now);
        s.on_segment(&Segment::new(SYN, 0, 0, vec![]), now);
        let mut bad = build(0x21, &[0; 8], 0xffff);
        bad[12..16].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(
            s.on_segment(&Segment::new(ACK | PSH | END, 0, 0, bad), now)
                .is_none()
        );
        assert_eq!(s.stats.refused, 1);
        assert!(s.received().is_empty());
    }

    #[test]
    fn repeated_tail_is_acked_again() {
        let now = Instant::now();
        let mut s = Session::new(Vec::new(), config(), now);
        let obj = build(0x21, &[1; 8], 0xffff);
        s.on_segment(&Segment::new(SYN, 0, 0, vec![]), now);
        let seg = Segment::new(ACK | PSH | END, 0, 0, obj.clone());
        assert_eq!(s.on_segment(&seg, now), Some(obj.clone()));
        s.take_output();
        assert_eq!(s.on_segment(&seg, now), None);
        let out = s.take_output();
        assert_eq!((out[0].flags, out[0].ack), (ACK, obj.len() as u32));
        assert_eq!(s.received().len(), 1);
    }

    #[test]
    fn verdict_holds_next_object() {
        let now = Instant::now();
        let mut s = Session::new(objects(6, &[8]), config(), now);
        let verdict = build(kind::VERDICT_REPLY, &[0; 8], 0xffff);
        s.on_segment(&Segment::new(SYN, 0, 0, vec![]), now);
        s.on_segment(&Segment::new(ACK | PSH | END, 0, 0, verdict), now);
        s.take_output();
        s.poll(now + Duration::from_millis(30));
        assert!(s.take_output().is_empty());
        s.poll(now + VERDICT_HOLD);
        assert_eq!(s.take_output()[0].flags, SYN);
    }
}
