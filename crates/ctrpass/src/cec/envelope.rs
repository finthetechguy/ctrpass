//! Envelopes: one or more containers of a single title, as they travel in an exchange.

use super::container::{self, Container, MIN_TOTAL};
use crate::Error;

pub const MAGIC: [u8; 2] = [0x61, 0x61];
pub const HEADER_LEN: usize = 0x14;
pub const MAX_LEN: usize = 1 << 20;
pub const MAX_CONTAINERS: usize = 64;

fn u32_at(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(b[at..at + 4].try_into().unwrap())
}

fn bad(m: String) -> Error {
    Error::Format(m)
}

/// Splits an envelope into its containers, checking every size before reading it.
pub fn split(buf: &[u8]) -> Result<Vec<Container>, Error> {
    if buf.len() > MAX_LEN {
        return Err(bad(format!("envelope of {} bytes is too large", buf.len())));
    }
    if buf.len() < HEADER_LEN + MIN_TOTAL || buf[..2] != MAGIC {
        return Err(bad("not a CEC envelope".into()));
    }
    let total = u32_at(buf, 0x04) as usize;
    let title = u32_at(buf, 0x08);
    let count = u32_at(buf, 0x10) as usize;
    if total != buf.len() {
        return Err(bad(format!(
            "{} bytes present, envelope says {total}",
            buf.len()
        )));
    }
    if count == 0 || count > MAX_CONTAINERS || count > (total - HEADER_LEN) / MIN_TOTAL {
        return Err(bad(format!("envelope claims {count} containers")));
    }
    let mut out = Vec::with_capacity(count);
    let mut at = HEADER_LEN;
    for i in 0..count {
        if at + container::HEADER_LEN > total {
            return Err(bad(format!(
                "envelope ends after {i} of {count} containers"
            )));
        }
        let size = u32_at(buf, at + 0x04) as usize;
        if size < MIN_TOTAL || at + size > total {
            return Err(bad(format!("container {i} overruns the envelope")));
        }
        let c = Container::parse(&buf[at..at + size])?;
        if c.title() != title {
            return Err(bad(format!(
                "container {i} is title {:08X} in an envelope for {title:08X}",
                c.title()
            )));
        }
        out.push(c);
        at += size;
    }
    if at != total {
        return Err(bad(format!(
            "{} bytes left after the containers",
            total - at
        )));
    }
    Ok(out)
}

/// Reads a bare container or an envelope.
pub fn containers(buf: &[u8]) -> Result<Vec<Container>, Error> {
    match buf.get(..2) {
        Some(m) if m == MAGIC => split(buf),
        Some(m) if m == container::MAGIC => Ok(vec![Container::parse(buf)?]),
        _ => Err(bad("not a CEC message".into())),
    }
}

/// Wraps containers of one title in an envelope.
pub fn build(containers: &[Container]) -> Result<Vec<u8>, Error> {
    let Some(first) = containers.first() else {
        return Err(bad("an envelope needs at least one container".into()));
    };
    if containers.len() > MAX_CONTAINERS {
        return Err(bad(format!("more than {MAX_CONTAINERS} containers")));
    }
    if containers.iter().any(|c| c.title() != first.title()) {
        return Err(bad("an envelope holds a single title".into()));
    }
    let total = HEADER_LEN + containers.iter().map(Container::len).sum::<usize>();
    if total > MAX_LEN {
        return Err(bad(format!("envelope of {total} bytes is too large")));
    }
    let mut out = Vec::with_capacity(total);
    out.extend_from_slice(&MAGIC);
    out.extend_from_slice(&[0, 0]);
    out.extend_from_slice(&(total as u32).to_le_bytes());
    out.extend_from_slice(&first.title().to_le_bytes());
    out.extend_from_slice(&[0; 4]);
    out.extend_from_slice(&(containers.len() as u32).to_le_bytes());
    for c in containers {
        out.extend_from_slice(c.as_bytes());
    }
    Ok(out)
}

/// The title an envelope declares, without checking the rest of it.
pub fn title(buf: &[u8]) -> Option<u32> {
    (buf.len() >= HEADER_LEN && buf[..2] == MAGIC).then(|| u32_at(buf, 0x08))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cec::container::tests::sample;

    #[test]
    fn round_trip() {
        let cs = vec![sample(9, 1), sample(9, 2)];
        let env = build(&cs).unwrap();
        assert_eq!(title(&env), Some(9));
        assert_eq!(split(&env).unwrap(), cs);
        assert_eq!(containers(&env).unwrap(), cs);
        assert_eq!(containers(cs[0].as_bytes()).unwrap(), cs[..1]);
    }

    #[test]
    fn rejects_bad_envelopes() {
        assert!(build(&[sample(9, 1), sample(8, 2)]).is_err());
        assert!(build(&[]).is_err());
        let env = build(&[sample(9, 1)]).unwrap();
        let mut b = env.clone();
        b[0x10] = 2;
        assert!(split(&b).is_err());
        let mut b = env.clone();
        b[0x08] = 8;
        assert!(split(&b).is_err());
        assert!(split(&env[..env.len() - 4]).is_err());
        let mut b = env.clone();
        b.extend_from_slice(&[0; 4]);
        b[4..8].copy_from_slice(&(env.len() as u32 + 4).to_le_bytes());
        assert!(split(&b).is_err());
    }
}
