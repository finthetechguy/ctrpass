//! Sending and receiving raw 802.11 frames.

use std::io;
use std::time::Duration;

use crate::wifi::frame::Mac;
use crate::wifi::radiotap::RxInfo;

#[cfg(target_os = "linux")]
pub mod linux;
pub mod pcap;

/// How long a backend leaves out signal readings after the card starts, while drivers that calibrate their noise floor settle.
pub const SIGNAL_SETTLE: Duration = Duration::from_secs(3);

/// Start of the names given to interfaces made for StreetPass.
pub const INTERFACE_PREFIX: &str = "ctrpass";

/// Transmit rates in units of 500 kbit/s.
pub const RATE_1M: u8 = 2;
pub const RATE_11M: u8 = 22;

/// A received frame without radiotap header or FCS.
#[derive(Clone, Debug)]
pub struct Received {
    pub info: RxInfo,
    pub frame: Vec<u8>,
}

pub trait Radio {
    /// Address the radio acknowledges frames for.
    fn mac(&self) -> Mac;
    fn set_channel(&mut self, channel: u8) -> io::Result<()>;
    /// Injects a frame given without FCS at `rate`, in units of 500 kbit/s.
    fn send(&mut self, frame: &[u8], rate: u8) -> io::Result<()>;
    /// Waits up to `timeout` for the next frame.
    fn recv(&mut self, timeout: Duration) -> io::Result<Option<Received>>;
}

impl<R: Radio + ?Sized> Radio for &mut R {
    fn mac(&self) -> Mac {
        (**self).mac()
    }
    fn set_channel(&mut self, channel: u8) -> io::Result<()> {
        (**self).set_channel(channel)
    }
    fn send(&mut self, frame: &[u8], rate: u8) -> io::Result<()> {
        (**self).send(frame, rate)
    }
    fn recv(&mut self, timeout: Duration) -> io::Result<Option<Received>> {
        (**self).recv(timeout)
    }
}
