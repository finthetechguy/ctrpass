//! Service records: the titles a console offers to exchange, and when two of them pair.

use std::fmt;

use crate::Error;

const WIDTH_MASK: u8 = 0x0f;
const MODE_SHIFT: u8 = 4;
const MODE_MASK: u8 = 0x30;

/// The mode a title that only receives is offered with, since it pairs with every sending mode.
pub const RECEIVE_MODE: u8 = 3;

/// One title's record: `u32be title | u8 flags | three argument arrays of the flags' width`.
#[derive(Clone, PartialEq, Eq)]
pub struct Service {
    pub title: u32,
    pub flags: u8,
    pub args: Vec<u8>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mismatch {
    Title,
    Width,
    Mode,
    /// The argument masks reject each other at this byte.
    Arguments(usize),
}

pub fn record_len(flags: u8) -> usize {
    5 + 3 * (flags & WIDTH_MASK) as usize
}

impl Service {
    pub fn new(title: u32, flags: u8, args: Vec<u8>) -> Result<Self, Error> {
        if title == 0 {
            return Err(Error::Format("service title ID is zero".into()));
        }
        if args.len() + 5 != record_len(flags) {
            return Err(Error::Format(format!(
                "service {title:08X} with flags {flags:#04x} needs {} argument bytes, got {}",
                record_len(flags) - 5,
                args.len()
            )));
        }
        Ok(Service { title, flags, args })
    }

    pub fn width(&self) -> usize {
        (self.flags & WIDTH_MASK) as usize
    }

    pub fn mode(&self) -> u8 {
        (self.flags & MODE_MASK) >> MODE_SHIFT
    }

    pub fn size(&self) -> usize {
        record_len(self.flags)
    }

    pub fn with_mode(&self, mode: u8) -> Self {
        Service {
            flags: (self.flags & !MODE_MASK) | ((mode & 3) << MODE_SHIFT),
            ..self.clone()
        }
    }

    pub fn write(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.title.to_be_bytes());
        out.push(self.flags);
        out.extend_from_slice(&self.args);
    }

    /// Checks whether a console would pair this local record with a peer's.
    pub fn pairs_with(&self, peer: &Service) -> Result<(), Mismatch> {
        if self.title != peer.title {
            return Err(Mismatch::Title);
        }
        if self.width() != peer.width() {
            return Err(Mismatch::Width);
        }
        if !modes_pair(self.mode(), peer.mode()) {
            return Err(Mismatch::Mode);
        }
        let n = self.width();
        let (la, lb, lc) = (&self.args[..n], &self.args[n..2 * n], &self.args[2 * n..]);
        let (pa, pb, pc) = (&peer.args[..n], &peer.args[n..2 * n], &peer.args[2 * n..]);
        for i in 0..n {
            if la[i] & lc[i] != la[i] & pb[i] || pa[i] & pc[i] != pa[i] & lb[i] {
                return Err(Mismatch::Arguments(i));
            }
        }
        Ok(())
    }
}

impl fmt::Debug for Service {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self}")
    }
}

impl fmt::Display for Service {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:08X}/{:#04x}", self.title, self.flags)?;
        if !self.args.is_empty() {
            f.write_str("/")?;
            for b in &self.args {
                write!(f, "{b:02x}")?;
            }
        }
        Ok(())
    }
}

/// Mode 0 pairs only with 0, 1 with 2, and 3 with any nonzero mode.
pub fn modes_pair(a: u8, b: u8) -> bool {
    if a == 0 || b == 0 {
        return a == b;
    }
    a == 3 || b == 3 || a + b == 3
}

pub fn encode(services: &[Service]) -> Vec<u8> {
    let mut out = Vec::new();
    for s in services {
        s.write(&mut out);
    }
    out
}

/// Decodes whole records, returning them with an error for any truncated tail.
pub fn decode(data: &[u8]) -> (Vec<Service>, Option<Error>) {
    let mut out = Vec::new();
    let mut at = 0;
    while at < data.len() {
        if at + 5 > data.len() {
            return (out, Some(Error::Format("truncated service record".into())));
        }
        let n = record_len(data[at + 4]);
        if at + n > data.len() {
            return (
                out,
                Some(Error::Format("truncated service arguments".into())),
            );
        }
        let title = u32::from_be_bytes(data[at..at + 4].try_into().unwrap());
        out.push(Service {
            title,
            flags: data[at + 4],
            args: data[at + 5..at + n].to_vec(),
        });
        at += n;
    }
    (out, None)
}

/// Keeps the records, in order, whose encoding fits in `budget` bytes.
pub fn fit(services: &[Service], budget: usize) -> Vec<Service> {
    let mut used = 0;
    services
        .iter()
        .filter(|s| {
            let fits = used + s.size() <= budget;
            if fits {
                used += s.size();
            }
            fits
        })
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn svc(title: u32, flags: u8, args: &[u8]) -> Service {
        Service::new(title, flags, args.to_vec()).unwrap()
    }

    #[test]
    fn round_trip_and_truncation() {
        let list = vec![
            svc(0x0002_0800, 0x30, &[]),
            svc(0x0003_0000, 0x22, &[1, 2, 3, 4, 5, 6]),
        ];
        let raw = encode(&list);
        assert_eq!(raw.len(), 5 + 11);
        assert_eq!(&raw[..5], &[0, 2, 8, 0, 0x30]);
        let (back, err) = decode(&raw);
        assert!(err.is_none());
        assert_eq!(back, list);
        let (part, err) = decode(&raw[..10]);
        assert_eq!(part.len(), 1);
        assert!(err.is_some());
        assert!(Service::new(1, 0x01, vec![0; 2]).is_err());
    }

    #[test]
    fn modes() {
        assert!(modes_pair(0, 0) && !modes_pair(0, 3));
        assert!(modes_pair(1, 2) && !modes_pair(1, 1) && !modes_pair(2, 2));
        assert!(modes_pair(3, 1) && modes_pair(2, 3) && modes_pair(3, 3));
        assert_eq!(svc(5, 0x01, &[0; 3]).with_mode(2).flags, 0x21);
    }

    #[test]
    fn pairing() {
        let a = svc(7, 0x31, &[0x01, 0x01, 0x01]);
        assert_eq!(a.pairs_with(&a), Ok(()));
        assert_eq!(
            a.pairs_with(&svc(7, 0x31, &[0x01, 0x00, 0x01])),
            Err(Mismatch::Arguments(0))
        );
        assert_eq!(a.pairs_with(&svc(8, 0x31, &[0; 3])), Err(Mismatch::Title));
        assert_eq!(a.pairs_with(&svc(7, 0x30, &[])), Err(Mismatch::Width));
        assert_eq!(a.pairs_with(&svc(7, 0x01, &[0; 3])), Err(Mismatch::Mode));
    }

    #[test]
    fn fit_keeps_order() {
        let list = vec![
            svc(1, 0x01, &[0; 3]),
            svc(2, 0x0f, &[0; 45]),
            svc(3, 0, &[]),
        ];
        let kept: Vec<u32> = fit(&list, 20).iter().map(|s| s.title).collect();
        assert_eq!(kept, vec![1, 3]);
    }
}
