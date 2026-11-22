//! CEC message containers: a fixed header, extension headers, a body and an HMAC trailer.

use std::fmt;

use hmac::{Hmac, Mac as _};
use sha2::Sha256;

use crate::Error;
use crate::keys::TitleKey;

pub const MAGIC: [u8; 2] = [0x60, 0x60];
pub const HEADER_LEN: usize = 0x70;
pub const TRAILER_LEN: usize = 0x20;
pub const MAX_BODY: usize = 0x18000;
pub const MAX_TOTAL: usize = 0x19000;
pub const MIN_TOTAL: usize = HEADER_LEN + 4 + TRAILER_LEN;
pub const MAX_EXT_HEADERS: usize = 16;

/// Extension header holding the service record a title is offered with.
pub const EXT_SERVICE: u32 = 1;
/// Extension header holding a 40x40 RGB565 icon.
pub const EXT_ICON: u32 = 2;
/// Extension header holding NUL-terminated UTF-16 text shown with the message.
pub const EXT_INFO_TEXT: u32 = 4;

pub const ICON_LEN: usize = 40 * 40 * 2;

const TOTAL_SIZE: usize = 0x04;
const HEADER_SIZE: usize = 0x08;
const BODY_SIZE: usize = 0x0c;
const TITLE: usize = 0x10;
const MESSAGE_ID: usize = 0x20;
const FLAGS: usize = 0x34;
const SEND_MODE: usize = 0x35;
const CREATED: usize = 0x60;
const SEND_COUNT: usize = 0x6c;
const FORWARD_COUNT: usize = 0x6d;

fn u32_at(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(b[at..at + 4].try_into().unwrap())
}

/// A date and time as the console's clock keeps it, without a time zone.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct Timestamp {
    pub year: u32,
    pub month: u8,
    pub day: u8,
    /// Days since Sunday.
    pub weekday: u8,
    pub hour: u8,
    pub minute: u8,
    pub second: u8,
    pub millisecond: u16,
}

impl Timestamp {
    pub const LEN: usize = 12;

    /// Converts seconds since the Unix epoch, taken as the console's local time.
    pub fn from_unix(secs: u64, millisecond: u16) -> Self {
        let days = (secs / 86400) as i64;
        let rem = secs % 86400;
        // Civil date from a day count, valid for every date after 1970.
        let z = days + 719_468;
        let era = z.div_euclid(146_097);
        let doe = z - era * 146_097;
        let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let day = (doy - (153 * mp + 2) / 5 + 1) as u8;
        let month = if mp < 10 { mp + 3 } else { mp - 9 } as u8;
        let year = (yoe + era * 400 + (month <= 2) as i64) as u32;
        Timestamp {
            year,
            month,
            day,
            weekday: ((days + 4) % 7) as u8,
            hour: (rem / 3600) as u8,
            minute: (rem / 60 % 60) as u8,
            second: (rem % 60) as u8,
            millisecond,
        }
    }

    pub fn parse(b: &[u8]) -> Self {
        Timestamp {
            year: u32_at(b, 0),
            month: b[4],
            day: b[5],
            weekday: b[6],
            hour: b[7],
            minute: b[8],
            second: b[9],
            millisecond: u16::from_le_bytes([b[10], b[11]]),
        }
    }

    pub fn to_bytes(self) -> [u8; Self::LEN] {
        let mut b = [0u8; Self::LEN];
        b[..4].copy_from_slice(&self.year.to_le_bytes());
        b[4..10].copy_from_slice(&[
            self.month,
            self.day,
            self.weekday,
            self.hour,
            self.minute,
            self.second,
        ]);
        b[10..].copy_from_slice(&self.millisecond.to_le_bytes());
        b
    }
}

impl fmt::Display for Timestamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{:04}-{:02}-{:02} {:02}:{:02}:{:02}.{:03}",
            self.year, self.month, self.day, self.hour, self.minute, self.second, self.millisecond
        )
    }
}

/// Lists every way `buf` fails to be one well-formed container, allowing trailing bytes unless `exact`.
pub fn problems(buf: &[u8], exact: bool) -> Vec<String> {
    if buf.len() < HEADER_LEN {
        return vec![format!("{} bytes is shorter than a header", buf.len())];
    }
    if buf[..2] != MAGIC {
        return vec!["no container magic".into()];
    }
    let mut out = Vec::new();
    let total = u32_at(buf, TOTAL_SIZE) as usize;
    let hsize = u32_at(buf, HEADER_SIZE) as usize;
    let bsize = u32_at(buf, BODY_SIZE) as usize;
    if !(MIN_TOTAL..=MAX_TOTAL).contains(&total) {
        out.push(format!("total size {total:#x} out of range"));
    }
    if exact && buf.len() != total {
        out.push(format!(
            "{} bytes present, total size is {total}",
            buf.len()
        ));
    } else if buf.len() < total {
        out.push(format!("truncated: {} of {total} bytes", buf.len()));
    }
    if bsize == 0 || bsize > MAX_BODY || bsize % 4 != 0 {
        out.push(format!(
            "body size {bsize:#x} is not 4-byte aligned in range"
        ));
    }
    if hsize < HEADER_LEN || hsize % 4 != 0 {
        out.push(format!("header size {hsize:#x} is invalid"));
    }
    if hsize + bsize + TRAILER_LEN != total {
        out.push("header, body and trailer sizes do not add up to the total".into());
    }
    if u32_at(buf, TITLE) == 0 {
        out.push("title ID is zero".into());
    }
    if buf[SEND_MODE] >= 4 {
        out.push(format!("send mode {} is not below 4", buf[SEND_MODE]));
    }
    if buf[SEND_COUNT] > 1 && buf[FORWARD_COUNT] > 1 {
        out.push("send count and forward count are both above 1".into());
    }
    if out.is_empty() {
        if let Err(e) = walk_ext_headers(&buf[..hsize]) {
            out.push(e);
        }
    }
    out
}

fn walk_ext_headers(header: &[u8]) -> Result<Vec<(u32, &[u8])>, String> {
    let mut out = Vec::new();
    let mut at = HEADER_LEN;
    while at < header.len() {
        if at + 8 > header.len() {
            return Err("extension headers overrun the header size".into());
        }
        let kind = u32_at(header, at);
        let size = u32_at(header, at + 4) as usize;
        if kind == 0 {
            return Err("extension header of type 0 before the body".into());
        }
        if out.len() == MAX_EXT_HEADERS {
            return Err(format!("more than {MAX_EXT_HEADERS} extension headers"));
        }
        let end = at + 8 + size.next_multiple_of(4);
        if end > header.len() {
            return Err("extension headers overrun the header size".into());
        }
        out.push((kind, &header[at + 8..at + 8 + size]));
        at = end;
    }
    Ok(out)
}

fn sign(key: &TitleKey, body: &[u8]) -> [u8; TRAILER_LEN] {
    let mut mac = <Hmac<Sha256> as hmac::Mac>::new_from_slice(key).unwrap();
    mac.update(body);
    mac.finalize().into_bytes().into()
}

/// A container whose structure has been checked.
#[derive(Clone, PartialEq, Eq)]
pub struct Container(Vec<u8>);

impl fmt::Debug for Container {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Container({:08X}/{:016x}, {} bytes)",
            self.title(),
            self.message_id(),
            self.0.len()
        )
    }
}

impl Container {
    /// Checks `buf`, which must hold exactly one container.
    pub fn parse(buf: &[u8]) -> Result<Self, Error> {
        let bad = problems(buf, true);
        if !bad.is_empty() {
            return Err(Error::Format(format!(
                "malformed CEC message: {}",
                bad.join("; ")
            )));
        }
        Ok(Container(buf.to_vec()))
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    pub fn into_bytes(self) -> Vec<u8> {
        self.0
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        false
    }

    pub fn title(&self) -> u32 {
        u32_at(&self.0, TITLE)
    }

    pub fn message_id(&self) -> u64 {
        u64::from_le_bytes(self.0[MESSAGE_ID..MESSAGE_ID + 8].try_into().unwrap())
    }

    pub fn set_message_id(&mut self, id: u64) {
        self.0[MESSAGE_ID..MESSAGE_ID + 8].copy_from_slice(&id.to_le_bytes());
    }

    pub fn flags(&self) -> u8 {
        self.0[FLAGS]
    }

    pub fn send_mode(&self) -> u8 {
        self.0[SEND_MODE]
    }

    pub fn send_count(&self) -> u8 {
        self.0[SEND_COUNT]
    }

    pub fn forward_count(&self) -> u8 {
        self.0[FORWARD_COUNT]
    }

    pub fn created(&self) -> Timestamp {
        Timestamp::parse(&self.0[CREATED..CREATED + Timestamp::LEN])
    }

    fn header_size(&self) -> usize {
        u32_at(&self.0, HEADER_SIZE) as usize
    }

    pub fn ext_headers(&self) -> Vec<(u32, &[u8])> {
        walk_ext_headers(&self.0[..self.header_size()]).unwrap_or_default()
    }

    pub fn ext_header(&self, kind: u32) -> Option<&[u8]> {
        self.ext_headers()
            .into_iter()
            .find(|(k, _)| *k == kind)
            .map(|(_, d)| d)
    }

    pub fn body(&self) -> &[u8] {
        let h = self.header_size();
        &self.0[h..self.0.len() - TRAILER_LEN]
    }

    pub fn trailer(&self) -> &[u8] {
        &self.0[self.0.len() - TRAILER_LEN..]
    }

    pub fn verify(&self, key: &TitleKey) -> bool {
        let mut mac = <Hmac<Sha256> as hmac::Mac>::new_from_slice(key).unwrap();
        mac.update(self.body());
        mac.verify_slice(self.trailer()).is_ok()
    }
}

/// Everything a new container is built from.
#[derive(Clone, Debug)]
pub struct Draft<'a> {
    pub title: u32,
    pub body: &'a [u8],
    pub ext_headers: &'a [(u32, &'a [u8])],
    pub message_id: u64,
    pub created: Timestamp,
    pub send_mode: u8,
    pub flags: u8,
}

impl Draft<'_> {
    /// Builds and signs the container.
    pub fn build(&self, key: &TitleKey) -> Result<Container, Error> {
        let fail = |m: String| Err(Error::Format(m));
        if self.body.is_empty() || self.body.len() > MAX_BODY || self.body.len() % 4 != 0 {
            return fail(format!(
                "body of {:#x} bytes is not 4-byte aligned in range",
                self.body.len()
            ));
        }
        if self.title == 0 {
            return fail("title ID is zero".into());
        }
        if self.send_mode >= 4 {
            return fail(format!("send mode {} is not below 4", self.send_mode));
        }
        if self.ext_headers.len() > MAX_EXT_HEADERS {
            return fail(format!("more than {MAX_EXT_HEADERS} extension headers"));
        }
        let mut ext = Vec::new();
        for &(kind, data) in self.ext_headers {
            if kind == 0 {
                return fail("extension header type 0 is reserved".into());
            }
            ext.extend_from_slice(&kind.to_le_bytes());
            ext.extend_from_slice(&(data.len() as u32).to_le_bytes());
            ext.extend_from_slice(data);
            ext.resize(ext.len().next_multiple_of(4), 0);
        }
        let header_size = HEADER_LEN + ext.len();
        let total = header_size + self.body.len() + TRAILER_LEN;
        if total > MAX_TOTAL {
            return fail(format!("message of {total:#x} bytes is too large"));
        }
        let mut b = vec![0u8; HEADER_LEN];
        b[..2].copy_from_slice(&MAGIC);
        b[TOTAL_SIZE..TOTAL_SIZE + 4].copy_from_slice(&(total as u32).to_le_bytes());
        b[HEADER_SIZE..HEADER_SIZE + 4].copy_from_slice(&(header_size as u32).to_le_bytes());
        b[BODY_SIZE..BODY_SIZE + 4].copy_from_slice(&(self.body.len() as u32).to_le_bytes());
        b[TITLE..TITLE + 4].copy_from_slice(&self.title.to_le_bytes());
        b[MESSAGE_ID..MESSAGE_ID + 8].copy_from_slice(&self.message_id.to_le_bytes());
        b[FLAGS] = self.flags;
        b[SEND_MODE] = self.send_mode;
        b[CREATED..CREATED + Timestamp::LEN].copy_from_slice(&self.created.to_bytes());
        b[SEND_COUNT] = 0xff;
        b[FORWARD_COUNT] = 1;
        b.extend_from_slice(&ext);
        b.extend_from_slice(self.body);
        b.extend_from_slice(&sign(key, self.body));
        Container::parse(&b)
    }
}

/// Encodes text for an info text extension header.
pub fn info_text(text: &str) -> Vec<u8> {
    let mut out: Vec<u8> = text.encode_utf16().flat_map(|u| u.to_le_bytes()).collect();
    out.extend_from_slice(&[0, 0]);
    out
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub const KEY: TitleKey = [7; 32];

    pub fn sample(title: u32, id: u64) -> Container {
        let text = info_text("hi");
        Draft {
            title,
            body: &[1, 2, 3, 4, 5, 6, 7, 8],
            ext_headers: &[(EXT_INFO_TEXT, &text)],
            message_id: id,
            created: Timestamp::from_unix(1_700_000_000, 250),
            send_mode: 1,
            flags: 3,
        }
        .build(&KEY)
        .unwrap()
    }

    #[test]
    fn build_parse_verify() {
        let c = sample(0x0002_0800, 0x1122_3344_5566_7788);
        assert_eq!(c.len(), HEADER_LEN + 8 + 8 + 8 + TRAILER_LEN);
        assert_eq!(
            (c.title(), c.message_id(), c.send_mode()),
            (0x0002_0800, 0x1122_3344_5566_7788, 1)
        );
        assert_eq!(c.body(), &[1, 2, 3, 4, 5, 6, 7, 8]);
        assert_eq!(
            c.ext_header(EXT_INFO_TEXT),
            Some(&[b'h', 0, b'i', 0, 0, 0][..])
        );
        assert!(c.verify(&KEY) && !c.verify(&[0; 32]));
        assert_eq!(Container::parse(c.as_bytes()).unwrap(), c);
    }

    #[test]
    fn problems_found() {
        let c = sample(5, 1);
        let mut b = c.as_bytes().to_vec();
        assert!(!problems(&b[..b.len() - 1], true).is_empty());
        b.push(0);
        assert!(!problems(&b, true).is_empty() && problems(&b, false).is_empty());
        let mut m = c.as_bytes().to_vec();
        m[SEND_MODE] = 4;
        assert!(Container::parse(&m).is_err());
        let mut m = c.as_bytes().to_vec();
        m[HEADER_LEN] = 0;
        assert!(Container::parse(&m).is_err());
    }

    #[test]
    fn timestamps() {
        let t = Timestamp::from_unix(1_700_000_000, 250);
        assert_eq!(t.to_string(), "2023-11-14 22:13:20.250");
        assert_eq!(t.weekday, 2);
        assert_eq!(Timestamp::parse(&t.to_bytes()), t);
        assert_eq!(
            Timestamp::from_unix(951_782_400, 0).to_string()[..10],
            *"2000-02-29"
        );
    }
}
