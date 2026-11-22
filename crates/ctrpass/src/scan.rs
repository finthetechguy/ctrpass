//! Finding nearby consoles by listening for their StreetPass probes on each channel.

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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Found {
    pub mac: Mac,
    pub token: Option<Token>,
    pub services: Vec<Service>,
    pub channel: Option<u8>,
    pub signal: Option<i8>,
    pub frames: u32,
}

/// Listens on each channel in turn, reporting a console when first heard or when its probe changes.
pub fn scan(
    radio: &mut dyn Radio,
    channels: &[u8],
    dwell: Duration,
    rounds: u32,
    stop: &AtomicBool,
    mut on_found: impl FnMut(&Found),
) -> io::Result<Vec<Found>> {
    let mut found: Vec<Found> = Vec::new();
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
                let heard = Found {
                    mac: h.addr2,
                    token: d.token,
                    services: d.services,
                    channel: rx.info.frequency.and_then(channel_of).or(Some(channel)),
                    signal: rx.info.signal,
                    frames: 1,
                };
                match found.iter_mut().find(|f| f.mac == heard.mac) {
                    Some(f) => {
                        let changed = (f.token, &f.services) != (heard.token, &heard.services);
                        *f = Found {
                            frames: f.frames + 1,
                            ..heard
                        };
                        if changed {
                            on_found(f);
                        }
                    }
                    None => {
                        on_found(&heard);
                        found.push(heard);
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
                return Ok(Some(Received {
                    info: Default::default(),
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
}
