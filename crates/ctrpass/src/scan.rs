//! Finding nearby consoles by listening for their StreetPass probes on each channel.

use std::collections::VecDeque;
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crate::radio::Radio;
use crate::streetpass::ie::{self, Kind};
use crate::streetpass::linkkey::Token;
use crate::streetpass::service::Service;
use crate::wifi::channel_of;
use crate::wifi::frame::Mac;

/// Channels consoles look for each other on.
pub const CHANNELS: [u8; 3] = [1, 6, 11];
pub const DWELL: Duration = Duration::from_secs(2);

/// How many recent readings a console's signal is the median of.
const SIGNAL_WINDOW: usize = 8;
/// How many readings are needed before a console's signal is given.
const SIGNAL_MIN_READINGS: usize = 3;
/// How far the median must move, in dB, to report a console again; cards jitter and step by a few dB, and 6 dB is about twice the distance.
const SIGNAL_STEP: i16 = 6;

/// Whether a reading in dBm could come from a real frame; some drivers report readings at or above 0 dBm while they settle after tuning.
fn plausible(dbm: i8) -> bool {
    (-100..0).contains(&dbm)
}

/// A console's recent signal readings.
#[derive(Default)]
struct Readings(VecDeque<i8>);

impl Readings {
    fn add(&mut self, dbm: Option<i8>) {
        if let Some(d) = dbm.filter(|&d| plausible(d)) {
            if self.0.len() == SIGNAL_WINDOW {
                self.0.pop_front();
            }
            self.0.push_back(d);
        }
    }

    fn median(&self) -> Option<i8> {
        if self.0.len() < SIGNAL_MIN_READINGS {
            return None;
        }
        let mut v: Vec<i8> = self.0.iter().copied().collect();
        v.sort_unstable();
        v.get(v.len() / 2).copied()
    }
}

/// A console's readings on each channel it was heard on.
#[derive(Default)]
struct Heard(Vec<(u8, Readings)>);

impl Heard {
    fn add(&mut self, channel: u8, dbm: Option<i8>) {
        match self.0.iter_mut().find(|(c, _)| *c == channel) {
            Some((_, r)) => r.add(dbm),
            None => {
                let mut r = Readings::default();
                r.add(dbm);
                self.0.push((channel, r));
            }
        }
    }

    /// The channel the console is strongest on, with its signal there.
    fn best(&self) -> Option<(u8, i8)> {
        self.0
            .iter()
            .filter_map(|(c, r)| r.median().map(|m| (*c, m)))
            .max_by_key(|&(_, m)| m)
    }
}

fn moved(old: Option<i8>, new: Option<i8>) -> bool {
    match (old, new) {
        (None, Some(_)) => true,
        (Some(a), Some(b)) => (i16::from(a) - i16::from(b)).abs() >= SIGNAL_STEP,
        _ => false,
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Found {
    pub mac: Mac,
    pub token: Option<Token>,
    pub services: Vec<Service>,
    pub channel: Option<u8>,
    /// The median of recent plausible readings on `channel`, in dBm.
    pub signal: Option<i8>,
    pub frames: u32,
}

/// Listens on each channel in turn, reporting a console when first heard, when its probe changes, or when its signal is first known or moves.
pub fn scan(
    radio: &mut dyn Radio,
    channels: &[u8],
    dwell: Duration,
    rounds: u32,
    stop: &AtomicBool,
    mut on_found: impl FnMut(&Found),
) -> io::Result<Vec<Found>> {
    let mut found: Vec<Found> = Vec::new();
    let mut readings: Vec<Heard> = Vec::new();
    'outer: for _ in 0..rounds {
        for &channel in channels {
            if stop.load(Ordering::Relaxed) {
                break 'outer;
            }
            radio.set_channel(channel)?;
            let until = Instant::now() + dwell;
            while let Some(rx) = radio.recv(until.saturating_duration_since(Instant::now()))? {
                let Ok(Some((h, Kind::Probe(d) | Kind::Offer(d)))) = ie::classify(&rx.frame) else {
                    continue;
                };
                let on = rx.info.frequency.and_then(channel_of).unwrap_or(channel);
                let mut heard = Found {
                    mac: h.addr2,
                    token: d.token,
                    services: d.services,
                    channel: Some(on),
                    signal: None,
                    frames: 1,
                };
                match found.iter().position(|f| f.mac == heard.mac) {
                    Some(i) => {
                        readings[i].add(on, rx.info.signal);
                        let f = &mut found[i];
                        let best = readings[i].best();
                        let changed = (f.token, &f.services) != (heard.token, &heard.services)
                            || moved(f.signal, best.map(|(_, m)| m));
                        let (channel, signal) = match best {
                            Some((c, m)) if changed || f.signal.is_none() => (Some(c), Some(m)),
                            _ if f.signal.is_some() => (f.channel, f.signal),
                            _ => (Some(on), None),
                        };
                        *f = Found {
                            frames: f.frames + 1,
                            channel,
                            signal,
                            ..heard
                        };
                        if changed {
                            on_found(f);
                        }
                    }
                    None => {
                        let mut r = Heard::default();
                        r.add(on, rx.info.signal);
                        if let Some((c, m)) = r.best() {
                            (heard.channel, heard.signal) = (Some(c), Some(m));
                        }
                        on_found(&heard);
                        found.push(heard);
                        readings.push(r);
                    }
                }
            }
        }
    }
    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::radio::Received;

    struct FakeRadio {
        channel: u8,
        probe: Vec<u8>,
        pending: u32,
        signals: Vec<Option<i8>>,
    }

    impl Radio for FakeRadio {
        fn mac(&self) -> Mac {
            [2; 6]
        }
        fn set_channel(&mut self, channel: u8) -> io::Result<()> {
            self.channel = channel;
            self.pending = 2;
            Ok(())
        }
        fn send(&mut self, _: &[u8], _: u8) -> io::Result<()> {
            Ok(())
        }
        fn recv(&mut self, _: Duration) -> io::Result<Option<Received>> {
            if self.channel == 6 && self.pending > 0 {
                self.pending -= 1;
                let mut info = crate::wifi::radiotap::RxInfo::default();
                if !self.signals.is_empty() {
                    info.signal = self.signals.remove(0);
                }
                return Ok(Some(Received {
                    info,
                    frame: self.probe.clone(),
                }));
            }
            Ok(None)
        }
    }

    #[test]
    fn finds_console_once() {
        let mac = [0x96, 0x58, 0xcb, 1, 2, 3];
        let services = vec![Service::new(0x0002_0800, 0x30, vec![]).unwrap()];
        let mut radio = FakeRadio {
            channel: 0,
            probe: ie::probe(mac, &[7; 8], &services, 1),
            pending: 0,
            signals: vec![],
        };
        let mut reports = 0;
        let found = scan(
            &mut radio,
            &CHANNELS,
            Duration::ZERO,
            2,
            &AtomicBool::new(false),
            |_| reports += 1,
        )
        .unwrap();
        assert_eq!(reports, 1);
        assert_eq!(found.len(), 1);
        let f = &found[0];
        assert_eq!(
            (f.mac, f.token, f.channel, f.frames),
            (mac, Some([7; 8]), Some(6), 4)
        );
        assert_eq!(f.services, services);
    }

    #[test]
    fn reports_again_when_signal_arrives() {
        let mac = [0x96, 0x58, 0xcb, 1, 2, 3];
        let mut radio = FakeRadio {
            channel: 0,
            probe: ie::probe(mac, &[7; 8], &[], 1),
            pending: 0,
            signals: vec![None, Some(-40), Some(-41), Some(-40)],
        };
        let mut signals = Vec::new();
        let found = scan(
            &mut radio,
            &CHANNELS,
            Duration::ZERO,
            2,
            &AtomicBool::new(false),
            |f| signals.push(f.signal),
        )
        .unwrap();
        assert_eq!(signals, vec![None, Some(-40)]);
        assert_eq!(found[0].signal, Some(-40));
    }

    #[test]
    fn ignores_settling_readings_and_follows_the_median() {
        let mac = [0x96, 0x58, 0xcb, 1, 2, 3];
        // Readings as a card gives them just after tuning, then settled ones, then a move closer.
        let readings = [
            2, 3, 0, -56, -59, -54, -55, -57, -41, -40, -42, -40, -41, -39,
        ];
        let mut radio = FakeRadio {
            channel: 0,
            probe: ie::probe(mac, &[7; 8], &[], 1),
            pending: 0,
            signals: readings.iter().map(|&r| Some(r)).collect(),
        };
        let mut signals = Vec::new();
        let found = scan(
            &mut radio,
            &CHANNELS,
            Duration::ZERO,
            8,
            &AtomicBool::new(false),
            |f| signals.push(f.signal),
        )
        .unwrap();
        assert_eq!(signals, vec![None, Some(-56), Some(-42)]);
        assert_eq!(found[0].signal, Some(-42));
    }

    #[test]
    fn steps_between_two_levels_are_not_reported() {
        let mac = [0x96, 0x58, 0xcb, 1, 2, 3];
        let mut radio = FakeRadio {
            channel: 0,
            probe: ie::probe(mac, &[7; 8], &[], 1),
            pending: 0,
            signals: [-52, -56].repeat(8).into_iter().map(Some).collect(),
        };
        let mut signals = Vec::new();
        scan(
            &mut radio,
            &CHANNELS,
            Duration::ZERO,
            8,
            &AtomicBool::new(false),
            |f| signals.push(f.signal),
        )
        .unwrap();
        assert_eq!(signals, vec![None, Some(-52)]);
    }

    #[test]
    fn strongest_channel_is_given() {
        let mut h = Heard::default();
        for _ in 0..3 {
            h.add(6, Some(-60));
            h.add(11, Some(-52));
            h.add(1, Some(-70));
        }
        assert_eq!(h.best(), Some((11, -52)));
        h.add(1, Some(5));
        assert_eq!(h.best(), Some((11, -52)));
    }

    #[test]
    fn median_of_recent_readings() {
        let mut r = Readings::default();
        assert_eq!(r.median(), None);
        r.add(Some(-60));
        r.add(Some(-50));
        assert_eq!(r.median(), None);
        for d in [5, -128, -70] {
            r.add(Some(d));
        }
        assert_eq!(r.median(), Some(-60));
        for _ in 0..SIGNAL_WINDOW {
            r.add(Some(-30));
        }
        assert_eq!(r.median(), Some(-30));
    }
}
