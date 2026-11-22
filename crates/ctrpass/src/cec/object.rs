//! Exchange objects: the typed records two consoles trade over a link before and around messages.

use super::container::Container;
use super::envelope;
use crate::Error;

pub const MAGIC: [u8; 2] = [0x63, 0x63];
pub const HEADER_LEN: usize = 0x10;

/// Object types, each answered with the type one above it.
pub mod kind {
    pub const SERVICES: u32 = 0x11;
    pub const CONSOLE_ID: u32 = 0x21;
    /// Purpose unknown; sent with the value 1 and answered with 2.
    pub const TYPE_23: u32 = 0x23;
    pub const OFFER: u32 = 0x31;
    pub const OFFER_REPLY: u32 = 0x32;
    /// The leading side's verdict on the other side's offer.
    pub const VERDICT: u32 = 0x33;
    /// The waiting side's verdict on the other side's offer.
    pub const VERDICT_REPLY: u32 = 0x34;
    pub const SESSION_OPEN: u32 = 0x41;
    pub const TYPE_43: u32 = 0x43;
    /// The titles and sizes of the envelopes that follow.
    pub const MESSAGES: u32 = 0x81;
    /// Sent by a console that went back to scanning.
    pub const SCANNING: u32 = 0xf2;
}

pub const VERDICT_ADMIT: u16 = 1;
pub const VERDICT_REFUSE: u16 = 2;

const LIST_MAGIC: [u8; 2] = [0x65, 0x65];
const LIST_HEADER: usize = 0x2c;
const LIST_ENTRY: usize = 0x20;
/// Most titles one offer or message list can name.
pub const MAX_TITLES: usize = 24;

fn u32_at(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(b[at..at + 4].try_into().unwrap())
}

pub fn build(kind: u32, payload: &[u8], flags: u16) -> Vec<u8> {
    let mut out = Vec::with_capacity(HEADER_LEN + payload.len());
    out.extend_from_slice(&MAGIC);
    out.extend_from_slice(&flags.to_le_bytes());
    out.extend_from_slice(&kind.to_le_bytes());
    out.extend_from_slice(&[0; 4]);
    out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    out.extend_from_slice(payload);
    out
}

/// The type of an exchange object, or `None` for anything else such as an envelope.
pub fn kind_of(obj: &[u8]) -> Option<u32> {
    (obj.len() >= HEADER_LEN && obj[..2] == MAGIC).then(|| u32_at(obj, 4))
}

pub fn payload(obj: &[u8]) -> &[u8] {
    &obj[HEADER_LEN.min(obj.len())..]
}

/// Returns `obj` with its type changed, as an answer that echoes the other side's object.
pub fn retype(obj: &[u8], kind: u32) -> Vec<u8> {
    let mut b = obj.to_vec();
    b[4..8].copy_from_slice(&kind.to_le_bytes());
    b
}

/// The total length an object or envelope declares in its first bytes.
pub fn declared_len(buf: &[u8]) -> Option<usize> {
    if buf.len() < HEADER_LEN {
        return None;
    }
    match [buf[0], buf[1]] {
        MAGIC => Some(u32_at(buf, 0x0c) as usize + HEADER_LEN),
        envelope::MAGIC => Some(u32_at(buf, 0x04) as usize),
        _ => None,
    }
}

/// One title in an offer or verdict list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Entry {
    pub title: u32,
    pub mode: u8,
    pub flags: u8,
    pub code: u16,
    /// Total bytes of the title's containers.
    pub size: u32,
    pub count: u32,
}

pub fn build_list(entries: &[Entry]) -> Result<Vec<u8>, Error> {
    if entries.len() > MAX_TITLES {
        return Err(Error::Format(format!(
            "a title list holds at most {MAX_TITLES} entries"
        )));
    }
    let len = LIST_HEADER + entries.len() * LIST_ENTRY;
    let mut r = vec![0u8; len];
    r[..2].copy_from_slice(&LIST_MAGIC);
    r[4..8].copy_from_slice(&(len as u32).to_le_bytes());
    r[8..12].copy_from_slice(&(entries.len() as u32).to_le_bytes());
    for (i, e) in entries.iter().enumerate() {
        let at = LIST_HEADER + i * LIST_ENTRY;
        r[at..at + 4].copy_from_slice(&e.title.to_le_bytes());
        r[at + 4] = e.mode;
        r[at + 5] = e.flags;
        r[at + 6..at + 8].copy_from_slice(&e.code.to_le_bytes());
        r[at + 8..at + 12].copy_from_slice(&e.size.to_le_bytes());
        r[at + 12..at + 16].copy_from_slice(&e.count.to_le_bytes());
    }
    Ok(r)
}

pub fn parse_list(list: &[u8]) -> Result<Vec<Entry>, Error> {
    if list.len() < LIST_HEADER || list[..2] != LIST_MAGIC {
        return Err(Error::Format("not a title list".into()));
    }
    let count = (u32_at(list, 8) as usize).min((list.len() - LIST_HEADER) / LIST_ENTRY);
    Ok((0..count)
        .map(|i| {
            let at = LIST_HEADER + i * LIST_ENTRY;
            Entry {
                title: u32_at(list, at),
                mode: list[at + 4],
                flags: list[at + 5],
                code: u16::from_le_bytes([list[at + 6], list[at + 7]]),
                size: u32_at(list, at + 8),
                count: u32_at(list, at + 12),
            }
        })
        .collect())
}

/// An offer or verdict object carrying no titles.
pub fn empty_list(kind: u32) -> Vec<u8> {
    build(kind, &build_list(&[]).unwrap(), 0xffff)
}

/// Sets each listed title's code in an offer or verdict object, leaving other entries as they are.
pub fn set_codes(obj: &[u8], code: impl Fn(u32) -> Option<u16>) -> Result<Vec<u8>, Error> {
    let entries = parse_list(payload(obj))?;
    let mut b = obj.to_vec();
    for (i, e) in entries.iter().enumerate() {
        if let Some(c) = code(e.title) {
            let at = HEADER_LEN + LIST_HEADER + i * LIST_ENTRY + 6;
            b[at..at + 2].copy_from_slice(&c.to_le_bytes());
        }
    }
    Ok(b)
}

fn message_list(envelopes: &[Vec<u8>]) -> Vec<u8> {
    let mut p = vec![0u8; 4 + MAX_TITLES * 8];
    p[..4].copy_from_slice(&(envelopes.len() as u32).to_le_bytes());
    for (i, env) in envelopes.iter().enumerate() {
        let at = 4 + i * 8;
        p[at..at + 4].copy_from_slice(&envelope::title(env).unwrap_or(0).to_le_bytes());
        p[at + 4..at + 8].copy_from_slice(&(env.len() as u32).to_le_bytes());
    }
    build(kind::MESSAGES, &p, 0)
}

/// Reads the titles and envelope sizes from a message list's payload.
pub fn parse_message_list(payload: &[u8]) -> Result<Vec<(u32, u32)>, Error> {
    if payload.len() < 4 {
        return Err(Error::Format("message list too short".into()));
    }
    let count = u32_at(payload, 0) as usize;
    if count > MAX_TITLES || payload.len() < 4 + count * 8 {
        return Err(Error::Format(format!("message list claims {count} titles")));
    }
    Ok((0..count)
        .map(|i| (u32_at(payload, 4 + i * 8), u32_at(payload, 8 + i * 8)))
        .collect())
}

/// The objects one side sends, ending in its envelopes; with no messages, those of a side that only receives.
pub fn outbox(messages: &[Container], console_id: u64) -> Result<Vec<Vec<u8>>, Error> {
    let mut groups: Vec<(u32, Vec<Container>)> = Vec::new();
    for c in messages {
        match groups.iter_mut().find(|(t, _)| *t == c.title()) {
            Some((_, cs)) => cs.push(c.clone()),
            None => groups.push((c.title(), vec![c.clone()])),
        }
    }
    if groups.len() > MAX_TITLES {
        return Err(Error::Format(format!(
            "at most {MAX_TITLES} titles can be sent at once"
        )));
    }
    let mut envelopes = Vec::with_capacity(groups.len());
    let mut offer = Vec::with_capacity(groups.len());
    for (title, cs) in &groups {
        envelopes.push(envelope::build(cs)?);
        offer.push(Entry {
            title: *title,
            mode: cs[0].send_mode(),
            flags: cs[0].flags(),
            code: 0,
            size: cs.iter().map(|c| c.len() as u32).sum(),
            count: cs.len() as u32,
        });
    }
    let verdict: Vec<Entry> = offer
        .iter()
        .map(|e| Entry {
            code: VERDICT_ADMIT,
            ..*e
        })
        .collect();
    let mut out = vec![
        build(kind::CONSOLE_ID, &console_id.to_le_bytes(), 0xffff),
        build(kind::TYPE_23, &1u32.to_le_bytes(), 0xffff),
        build(kind::OFFER, &build_list(&offer)?, 0xffff),
        build(kind::VERDICT, &build_list(&verdict)?, 0xffff),
        message_list(&envelopes),
    ];
    out.extend(envelopes);
    Ok(out)
}

/// The outcome of applying the other side's verdict to an outbox.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Verdict {
    pub objects: Vec<Vec<u8>>,
    pub admitted: Vec<u32>,
    pub refused: Vec<u32>,
}

/// Keeps only the envelopes of titles the verdict admits, and rebuilds the message list to match.
pub fn apply_verdict(objects: &[Vec<u8>], verdict: &[u8]) -> Result<Verdict, Error> {
    if !matches!(kind_of(verdict), Some(kind::VERDICT | kind::VERDICT_REPLY)) {
        return Err(Error::Format("not a verdict object".into()));
    }
    let mut admitted: Vec<u32> = Vec::new();
    for e in parse_list(payload(verdict))? {
        if e.code == VERDICT_ADMIT && !admitted.contains(&e.title) {
            admitted.push(e.title);
        }
    }
    let Some(at) = objects
        .iter()
        .position(|o| kind_of(o) == Some(kind::MESSAGES))
    else {
        return Err(Error::Format("no message list to rebuild".into()));
    };
    let tail = &objects[at + 1..];
    let held: Vec<u32> = tail.iter().filter_map(|o| envelope::title(o)).collect();
    let refused: Vec<u32> = held
        .iter()
        .copied()
        .filter(|t| !admitted.contains(t))
        .collect();
    let keep: Vec<Vec<u8>> = admitted
        .iter()
        .flat_map(|t| tail.iter().filter(move |o| envelope::title(o) == Some(*t)))
        .cloned()
        .collect();
    let mut out = objects[..at].to_vec();
    out.push(message_list(&keep));
    out.extend(keep);
    out.extend(
        tail.iter()
            .filter(|o| envelope::title(o).is_none())
            .cloned(),
    );
    Ok(Verdict {
        objects: out,
        admitted: admitted.into_iter().filter(|t| held.contains(t)).collect(),
        refused,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cec::container::tests::sample;

    #[test]
    fn object_header() {
        let o = build(kind::CONSOLE_ID, &[1, 2, 3, 4, 5, 6, 7, 8], 0xffff);
        assert_eq!(
            &o[..HEADER_LEN],
            &[
                0x63, 0x63, 0xff, 0xff, 0x21, 0, 0, 0, 0, 0, 0, 0, 8, 0, 0, 0
            ]
        );
        assert_eq!(kind_of(&o), Some(kind::CONSOLE_ID));
        assert_eq!(declared_len(&o), Some(o.len()));
        assert_eq!(kind_of(&retype(&o, 0x22)), Some(0x22));
    }

    #[test]
    fn list_round_trip() {
        let e = Entry {
            title: 0x0002_0800,
            mode: 3,
            flags: 3,
            code: 1,
            size: 0x200,
            count: 2,
        };
        let l = build_list(&[e, e]).unwrap();
        assert_eq!(l.len(), 0x2c + 0x40);
        assert_eq!(parse_list(&l).unwrap(), vec![e, e]);
        assert!(build_list(&[e; 25]).is_err());
    }

    #[test]
    fn outbox_layout() {
        let msgs = [sample(5, 1), sample(6, 2), sample(5, 3)];
        let objs = outbox(&msgs, 0xabcd).unwrap();
        let kinds: Vec<_> = objs.iter().map(|o| kind_of(o)).collect();
        assert_eq!(
            &kinds[..5],
            &[Some(0x21), Some(0x23), Some(0x31), Some(0x33), Some(0x81)]
        );
        assert_eq!(objs.len(), 7);
        let offer = parse_list(payload(&objs[2])).unwrap();
        assert_eq!((offer[0].title, offer[0].count, offer[0].mode), (5, 2, 1));
        assert_eq!(offer[0].size as usize, msgs[0].len() * 2);
        assert_eq!(
            parse_list(payload(&objs[3])).unwrap()[1].code,
            VERDICT_ADMIT
        );
        let list = payload(&objs[4]);
        assert_eq!(list.len(), 4 + 24 * 8);
        assert_eq!(u32_at(list, 0), 2);
        assert_eq!(u32_at(list, 16) as usize, objs[6].len());
        assert_eq!(
            parse_message_list(list).unwrap(),
            vec![(5, objs[5].len() as u32), (6, objs[6].len() as u32)]
        );
        assert_eq!(envelope::split(&objs[5]).unwrap().len(), 2);

        let empty = outbox(&[], 1).unwrap();
        assert_eq!(empty.len(), 5);
        assert_eq!(u32_at(payload(&empty[4]), 0), 0);
    }

    #[test]
    fn verdict_cuts_refused_titles() {
        let objs = outbox(&[sample(5, 1), sample(6, 2)], 1).unwrap();
        let answer = set_codes(&retype(&objs[2], kind::VERDICT_REPLY), |t| {
            Some(if t == 6 {
                VERDICT_ADMIT
            } else {
                VERDICT_REFUSE
            })
        })
        .unwrap();
        let v = apply_verdict(&objs, &answer).unwrap();
        assert_eq!((v.admitted, v.refused), (vec![6], vec![5]));
        assert_eq!(v.objects.len(), 6);
        assert_eq!(envelope::title(&v.objects[5]), Some(6));
        assert_eq!(u32_at(payload(&v.objects[4]), 0), 1);
        assert!(apply_verdict(&objs, &objs[0]).is_err());
    }
}
