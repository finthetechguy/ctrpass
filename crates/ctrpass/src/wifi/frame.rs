//! 802.11 MAC headers and information elements.

use crate::Error;

pub type Mac = [u8; 6];

pub const BROADCAST: Mac = [0xff; 6];
pub const HEADER_LEN: usize = 24;

pub const TYPE_MGMT: u8 = 0;
pub const TYPE_DATA: u8 = 2;

pub const FC_TO_DS: u16 = 0x0100;
pub const FC_FROM_DS: u16 = 0x0200;
pub const FC_RETRY: u16 = 0x0800;
pub const FC_PROTECTED: u16 = 0x4000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Header {
    pub fc: u16,
    pub duration: u16,
    pub addr1: Mac,
    pub addr2: Mac,
    pub addr3: Mac,
    pub seq_ctrl: u16,
}

impl Header {
    pub fn new(ftype: u8, subtype: u8, flags: u16, addr1: Mac, addr2: Mac, addr3: Mac) -> Self {
        let fc = ((subtype as u16) << 4) | ((ftype as u16) << 2) | flags;
        Header {
            fc,
            duration: 0,
            addr1,
            addr2,
            addr3,
            seq_ctrl: 0,
        }
    }

    pub fn parse(frame: &[u8]) -> Result<Self, Error> {
        if frame.len() < HEADER_LEN {
            return Err(Error::Format("802.11 frame too short".into()));
        }
        let mac = |at: usize| -> Mac { frame[at..at + 6].try_into().unwrap() };
        Ok(Header {
            fc: u16::from_le_bytes([frame[0], frame[1]]),
            duration: u16::from_le_bytes([frame[2], frame[3]]),
            addr1: mac(4),
            addr2: mac(10),
            addr3: mac(16),
            seq_ctrl: u16::from_le_bytes([frame[22], frame[23]]),
        })
    }

    pub fn write(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.fc.to_le_bytes());
        out.extend_from_slice(&self.duration.to_le_bytes());
        out.extend_from_slice(&self.addr1);
        out.extend_from_slice(&self.addr2);
        out.extend_from_slice(&self.addr3);
        out.extend_from_slice(&self.seq_ctrl.to_le_bytes());
    }

    pub fn ftype(&self) -> u8 {
        ((self.fc >> 2) & 3) as u8
    }

    pub fn subtype(&self) -> u8 {
        ((self.fc >> 4) & 0xf) as u8
    }

    pub fn is(&self, ftype: u8, subtype: u8) -> bool {
        self.fc & 3 == 0 && self.ftype() == ftype && self.subtype() == subtype
    }

    pub fn protected(&self) -> bool {
        self.fc & FC_PROTECTED != 0
    }

    pub fn sequence(&self) -> u16 {
        self.seq_ctrl >> 4
    }

    pub fn set_sequence(&mut self, seq: u16) {
        self.seq_ctrl = (seq & 0x0fff) << 4;
    }
}

/// Iterates over complete information elements, stopping at the first truncated one.
pub fn elements(mut body: &[u8]) -> impl Iterator<Item = (u8, &[u8])> {
    std::iter::from_fn(move || {
        if body.len() < 2 || body.len() < 2 + body[1] as usize {
            return None;
        }
        let (id, len) = (body[0], body[1] as usize);
        let data = &body[2..2 + len];
        body = &body[2 + len..];
        Some((id, data))
    })
}

pub fn push_element(out: &mut Vec<u8>, id: u8, data: &[u8]) {
    debug_assert!(data.len() <= 255);
    out.push(id);
    out.push(data.len() as u8);
    out.extend_from_slice(data);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_round_trip() {
        let b: Mac = [0x02, 0, 0, 0, 0, 0xb];
        let mut h = Header::new(TYPE_DATA, 0, FC_PROTECTED, BROADCAST, b, b);
        h.set_sequence(0x123);
        let mut out = Vec::new();
        h.write(&mut out);
        assert_eq!(&out[..2], &[0x08, 0x40]);
        let p = Header::parse(&out).unwrap();
        assert_eq!(p, h);
        assert!(p.is(TYPE_DATA, 0) && p.protected());
        assert_eq!(p.sequence(), 0x123);
    }

    #[test]
    fn truncated_elements_stop() {
        let ies: Vec<_> = elements(&[0, 1, 9, 3, 5, 1]).collect();
        assert_eq!(ies, vec![(0, &[9u8][..])]);
    }
}
