//! YY segments: the transport inside a link's encrypted data frames.

pub const FIN: u8 = 0x01;
pub const SYN: u8 = 0x02;
pub const RST: u8 = 0x04;
pub const PSH: u8 = 0x08;
pub const ACK: u8 = 0x10;
/// Marks the last segment of an object or of a receive window.
pub const END: u8 = 0x20;

const MAGIC: u16 = 0x5959;
const CHECK_A: u16 = 0xdead;
const CHECK_B: u16 = 0xbeaf;
pub const HEADER_LEN: usize = 0x18;
/// The header length the length field counts, which includes bytes before the YY header.
const COUNTED_HEADER: usize = 0x30;
const DATA_OFFSET_WORDS: u8 = (COUNTED_HEADER / 4) as u8;
/// Largest payload in one segment.
pub const MAX_SEGMENT: usize = 0x5ac;
/// Receive window both sides advertise.
pub const WINDOW: u16 = 0x38b8;

/// LLC/SNAP header for IPv4 and two padding bytes, which precede the YY header.
const PREFIX: [u8; 10] = [0xaa, 0xaa, 0x03, 0x00, 0x00, 0x00, 0x08, 0x00, 0x00, 0x00];
const HEADER_AT: [usize; 4] = [PREFIX.len(), 0x18, 0, 8];

#[derive(Clone, PartialEq, Eq)]
pub struct Segment {
    pub flags: u8,
    pub seq: u32,
    pub ack: u32,
    pub window: u16,
    pub payload: Vec<u8>,
}

impl std::fmt::Debug for Segment {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Segment({} seq={:#x} ack={:#x} win={} len={})",
            flag_names(self.flags),
            self.seq,
            self.ack,
            self.window,
            self.payload.len()
        )
    }
}

pub fn flag_names(flags: u8) -> String {
    let names = [
        (FIN, "FIN"),
        (SYN, "SYN"),
        (RST, "RST"),
        (PSH, "PSH"),
        (ACK, "ACK"),
        (END, "END"),
    ];
    let set: Vec<&str> = names
        .iter()
        .filter(|(b, _)| flags & b != 0)
        .map(|(_, n)| *n)
        .collect();
    if set.is_empty() {
        "-".into()
    } else {
        set.join("|")
    }
}

fn u16_at(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([b[at], b[at + 1]])
}

fn u32_at(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(b[at..at + 4].try_into().unwrap())
}

impl Segment {
    pub fn new(flags: u8, seq: u32, ack: u32, payload: Vec<u8>) -> Self {
        Segment {
            flags,
            seq,
            ack,
            window: WINDOW,
            payload,
        }
    }

    /// Encodes the segment as the plaintext of a data frame.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(PREFIX.len() + HEADER_LEN + self.payload.len());
        out.extend_from_slice(&PREFIX);
        out.extend_from_slice(&MAGIC.to_le_bytes());
        out.extend_from_slice(&((COUNTED_HEADER + self.payload.len()) as u16).to_le_bytes());
        out.extend_from_slice(&CHECK_A.to_le_bytes());
        out.extend_from_slice(&CHECK_B.to_le_bytes());
        out.extend_from_slice(&self.seq.to_le_bytes());
        out.extend_from_slice(&self.ack.to_le_bytes());
        out.push(DATA_OFFSET_WORDS);
        out.push(self.flags);
        out.extend_from_slice(&self.window.to_le_bytes());
        out.extend_from_slice(&[0; 4]);
        out.extend_from_slice(&self.payload);
        out
    }

    /// Decodes a data frame's plaintext, returning `None` unless it holds one valid segment.
    pub fn parse(plain: &[u8]) -> Option<Self> {
        let at = HEADER_AT.into_iter().find(|&at| {
            at + HEADER_LEN <= plain.len()
                && u16_at(plain, at) == MAGIC
                && u16_at(plain, at + 4) == CHECK_A
                && u16_at(plain, at + 6) == CHECK_B
        })?;
        let h = &plain[at..at + HEADER_LEN];
        let length = u16_at(h, 2) as usize;
        let header_bytes = h[0x10] as usize * 4;
        if header_bytes < HEADER_LEN || length < header_bytes {
            return None;
        }
        let len = length - header_bytes;
        if len > MAX_SEGMENT {
            return None;
        }
        // The data offset counts from a point 0x18 bytes before the header when there is room for one.
        let start = match at.checked_sub(HEADER_LEN) {
            Some(base) => base + header_bytes,
            None => at + HEADER_LEN,
        };
        let payload = plain.get(start..start + len)?.to_vec();
        Some(Segment {
            flags: h[0x11],
            seq: u32_at(h, 8),
            ack: u32_at(h, 12),
            window: u16_at(h, 0x12),
            payload,
        })
    }
}

/// Splits an object into `(flags, seq, payload)` segments, ending each window and the object with END.
pub fn segments(obj: &[u8]) -> Vec<(u8, u32, Vec<u8>)> {
    if obj.is_empty() {
        return vec![(ACK | PSH | END, 0, Vec::new())];
    }
    obj.chunks(MAX_SEGMENT)
        .enumerate()
        .map(|(i, chunk)| {
            let seq = i * MAX_SEGMENT;
            let end = seq + chunk.len();
            let mut flags = ACK;
            if end == obj.len() || end % WINDOW as usize == 0 {
                flags |= PSH | END;
            }
            (flags, seq as u32, chunk.to_vec())
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let s = Segment::new(ACK | PSH | END, 0x1234, 0x10, vec![1, 2, 3]);
        let b = s.to_bytes();
        assert_eq!(
            &b[10..34],
            &[
                0x59, 0x59, 0x33, 0x00, 0xad, 0xde, 0xaf, 0xbe, 0x34, 0x12, 0, 0, 0x10, 0, 0, 0,
                12, 0x38, 0xb8, 0x38, 0, 0, 0, 0
            ]
        );
        assert_eq!(Segment::parse(&b), Some(s));
        assert_eq!(
            format!("{:?}", Segment::parse(&b).unwrap())
                .split(' ')
                .next(),
            Some("Segment(PSH|ACK|END")
        );
    }

    #[test]
    fn rejects_bad_segments() {
        let mut b = Segment::new(ACK, 0, 0, vec![0; 8]).to_bytes();
        assert!(Segment::parse(&b[..b.len() - 1]).is_none());
        b[14] = 0;
        assert!(Segment::parse(&b).is_none());
        let mut big = Segment::new(ACK, 0, 0, vec![0; MAX_SEGMENT + 1]).to_bytes();
        assert!(Segment::parse(&big).is_none());
        big.truncate(10);
        assert!(Segment::parse(&big).is_none());
    }

    #[test]
    fn segmenting() {
        let obj = vec![0u8; WINDOW as usize + 100];
        let segs = segments(&obj);
        assert_eq!(segs.len(), 11);
        assert!(segs[..9].iter().all(|s| s.0 == ACK));
        assert_eq!(segs[9].0, ACK | PSH | END);
        assert_eq!(segs[10], (ACK | PSH | END, WINDOW as u32, vec![0; 100]));
        assert_eq!(segments(&[]), vec![(ACK | PSH | END, 0, vec![])]);
    }
}
