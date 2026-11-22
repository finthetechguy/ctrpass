//! StreetPass discovery frames and the Nintendo vendor element they carry.

use super::linkkey::Token;
use super::service::{self, Service};
use crate::Error;
use crate::wifi::frame::{BROADCAST, HEADER_LEN, Header, Mac, TYPE_MGMT, elements, push_element};

pub const SSID: &[u8] = b"Nintendo_3DS_continuous_scan_000";
pub const NINTENDO_OUI: [u8; 3] = [0x00, 0x1f, 0x32];
const VENDOR_SUBTYPE: u8 = 1;

const PROBE_REQUEST: u8 = 4;
const PROBE_RESPONSE: u8 = 5;
const ACTION: u8 = 13;

const IE_SSID: u8 = 0;
const IE_RATES: u8 = 1;
const IE_DS: u8 = 3;
const IE_IBSS: u8 = 6;
const IE_EXT_RATES: u8 = 50;
const IE_VENDOR: u8 = 221;

const RATES: [u8; 8] = [0x82, 0x84, 0x8b, 0x0c, 0x12, 0x96, 0x18, 0x24];
const EXT_RATES: [u8; 4] = [0x30, 0x48, 0x60, 0x6c];

const CAP_IBSS: u16 = 0x0002;
const CAP_PRIVACY: u16 = 0x0010;
const BEACON_INTERVAL: u16 = 100;

const DIRECTED_PROBE_DURATION: u16 = 0x013a;
const ACTION_DURATION: u16 = 0x00d5;
const ACTION_CATEGORY: u8 = 0x7f;

/// Elements that close a link announcement: a WMM information element and one vendor element.
const LINK_ELEMENTS: [u8; 22] = [
    0xdd, 0x07, 0x00, 0x50, 0xf2, 0x02, 0x00, 0x01, 0x00, 0xdd, 0x0b, 0x00, 0x03, 0x7f, 0x01, 0x01,
    0x02, 0x01, 0x00, 0x00, 0x0a, 0x00,
];

const TLV_MAX: usize = 255;
const TLV_SERVICES: (u8, u8) = (1, 1);
const TLV_TOKEN: (u8, u8) = (0, 0xf);

/// Room for service records in one vendor element, after the OUI, subtype, token and one TLV header.
pub const DISCOVERY_BUDGET: usize = 255 - 4 - 10 - 2;

/// What a probe request or response says about the console that sent it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Discovery {
    pub ssid: Vec<u8>,
    pub channel: Option<u8>,
    pub token: Option<Token>,
    pub services: Vec<Service>,
    /// Set when the service records did not decode cleanly.
    pub problem: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Kind {
    /// A broadcast probe looking for peers.
    Probe(Discovery),
    /// A probe response from a console that offers to host a link.
    Offer(Discovery),
    /// A probe asking a chosen peer to announce its link.
    DirectedProbe,
    /// The vendor action frame a console sends after accepting an offer.
    Accept,
    /// A probe response announcing that the encrypted link is up.
    LinkUp,
}

fn tlv_header(out: &mut Vec<u8>, (a, b): (u8, u8), len: usize) {
    let h = (len as u16) << 8 | (b as u16) << 4 | a as u16;
    out.extend_from_slice(&h.to_le_bytes());
}

/// Service records and the token as TLVs, never splitting a record across two TLVs.
fn push_records(body: &mut Vec<u8>, token: &Token, services: &[Service]) {
    let mut chunk = Vec::new();
    for s in services {
        if !chunk.is_empty() && chunk.len() + s.size() > TLV_MAX {
            tlv_header(body, TLV_SERVICES, chunk.len());
            body.append(&mut chunk);
        }
        s.write(&mut chunk);
    }
    tlv_header(body, TLV_SERVICES, chunk.len());
    body.append(&mut chunk);
    tlv_header(body, TLV_TOKEN, token.len());
    body.extend_from_slice(token);
}

fn vendor_element(token: &Token, services: &[Service]) -> Vec<u8> {
    let mut body = NINTENDO_OUI.to_vec();
    body.push(VENDOR_SUBTYPE);
    push_records(&mut body, token, &service::fit(services, DISCOVERY_BUDGET));
    body
}

/// The payload of the services object sent inside a link, which carries every record.
pub fn link_records(token: &Token, services: &[Service]) -> Vec<u8> {
    let mut body = Vec::new();
    push_records(&mut body, token, services);
    body
}

fn parse_vendor(data: &[u8], into: &mut Discovery) {
    if data.len() < 4 || data[..3] != NINTENDO_OUI {
        return;
    }
    let mut records = Vec::new();
    let mut rest = &data[4..];
    while rest.len() >= 2 {
        let h = u16::from_le_bytes([rest[0], rest[1]]);
        let kind = ((h & 0xf) as u8, ((h >> 4) & 0xf) as u8);
        let len = (h >> 8) as usize;
        if rest.len() < 2 + len {
            into.problem = Some("truncated vendor record".into());
            break;
        }
        let value = &rest[2..2 + len];
        match kind {
            TLV_SERVICES => records.extend_from_slice(value),
            TLV_TOKEN if len == 8 => into.token = Some(value.try_into().unwrap()),
            _ => {}
        }
        rest = &rest[2 + len..];
    }
    let (services, err) = service::decode(&records);
    into.services = services;
    if let Some(e) = err {
        into.problem = Some(e.to_string());
    }
}

fn mgmt(subtype: u8, duration: u16, addr1: Mac, addr2: Mac, addr3: Mac, seq: u16) -> Vec<u8> {
    let mut h = Header::new(TYPE_MGMT, subtype, 0, addr1, addr2, addr3);
    h.duration = duration;
    h.set_sequence(seq);
    let mut out = Vec::with_capacity(128);
    h.write(&mut out);
    out
}

fn push_rates(out: &mut Vec<u8>) {
    push_element(out, IE_RATES, &RATES);
}

fn push_ibss(out: &mut Vec<u8>, channel: u8) {
    push_rates(out);
    push_element(out, IE_DS, &[channel]);
    push_element(out, IE_IBSS, &[0, 0]);
    push_element(out, IE_EXT_RATES, &EXT_RATES);
}

/// The broadcast probe that announces a console's token and services.
pub fn probe(src: Mac, token: &Token, services: &[Service], seq: u16) -> Vec<u8> {
    let mut out = mgmt(PROBE_REQUEST, 0, BROADCAST, src, BROADCAST, seq);
    push_element(&mut out, IE_SSID, SSID);
    push_rates(&mut out);
    push_element(&mut out, IE_EXT_RATES, &EXT_RATES);
    push_element(&mut out, IE_VENDOR, &vendor_element(token, services));
    out
}

pub fn directed_probe(src: Mac, dst: Mac, seq: u16) -> Vec<u8> {
    let mut out = mgmt(PROBE_REQUEST, DIRECTED_PROBE_DURATION, dst, src, dst, seq);
    push_element(&mut out, IE_SSID, &[]);
    push_rates(&mut out);
    push_element(&mut out, IE_EXT_RATES, &EXT_RATES);
    out
}

pub fn accept(src: Mac, dst: Mac, seq: u16) -> Vec<u8> {
    let mut out = mgmt(ACTION, ACTION_DURATION, dst, src, dst, seq);
    out.push(ACTION_CATEGORY);
    out.extend_from_slice(&NINTENDO_OUI);
    out.extend_from_slice(&[0, 0]);
    out
}

fn probe_response(src: Mac, dst: Mac, seq: u16, tsf: u64, capability: u16) -> Vec<u8> {
    let mut out = mgmt(PROBE_RESPONSE, 0, dst, src, src, seq);
    out.extend_from_slice(&tsf.to_le_bytes());
    out.extend_from_slice(&BEACON_INTERVAL.to_le_bytes());
    out.extend_from_slice(&capability.to_le_bytes());
    out
}

/// The probe response that offers to host a link with the console whose probe was heard.
pub fn offer(
    src: Mac,
    dst: Mac,
    token: &Token,
    services: &[Service],
    channel: u8,
    seq: u16,
    tsf: u64,
) -> Vec<u8> {
    let mut out = probe_response(src, dst, seq, tsf, CAP_IBSS);
    push_element(&mut out, IE_SSID, SSID);
    push_ibss(&mut out, channel);
    push_element(&mut out, IE_VENDOR, &vendor_element(token, services));
    out
}

pub fn link_up(src: Mac, dst: Mac, channel: u8, seq: u16, tsf: u64) -> Vec<u8> {
    let mut out = probe_response(src, dst, seq, tsf, CAP_IBSS | CAP_PRIVACY);
    push_element(&mut out, IE_SSID, &[]);
    push_ibss(&mut out, channel);
    out.extend_from_slice(&LINK_ELEMENTS);
    out
}

fn discovery(ies: &[u8]) -> Discovery {
    let mut d = Discovery::default();
    for (id, data) in elements(ies) {
        match id {
            IE_SSID => d.ssid = data.to_vec(),
            IE_DS if data.len() == 1 => d.channel = Some(data[0]),
            IE_VENDOR => parse_vendor(data, &mut d),
            _ => {}
        }
    }
    d
}

/// Recognises the StreetPass discovery frames.
pub fn classify(frame: &[u8]) -> Result<Option<(Header, Kind)>, Error> {
    let h = Header::parse(frame)?;
    let body = &frame[HEADER_LEN..];
    let kind = if h.is(TYPE_MGMT, PROBE_REQUEST) {
        let d = discovery(body);
        if d.ssid == SSID {
            Kind::Probe(d)
        } else if d.ssid.is_empty() && h.addr1 != BROADCAST {
            Kind::DirectedProbe
        } else {
            return Ok(None);
        }
    } else if h.is(TYPE_MGMT, PROBE_RESPONSE) && body.len() >= 12 {
        let capability = u16::from_le_bytes([body[10], body[11]]);
        let d = discovery(&body[12..]);
        if d.ssid == SSID && d.token.is_some() {
            Kind::Offer(d)
        } else if d.ssid.is_empty() && capability & CAP_PRIVACY != 0 {
            Kind::LinkUp
        } else {
            return Ok(None);
        }
    } else if h.is(TYPE_MGMT, ACTION)
        && body.len() >= 4
        && body[0] == ACTION_CATEGORY
        && body[1..4] == NINTENDO_OUI
    {
        Kind::Accept
    } else {
        return Ok(None);
    };
    Ok(Some((h, kind)))
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: Mac = [0xba, 0xae, 0x6e, 1, 2, 3];
    const B: Mac = [0x96, 0x58, 0xcb, 4, 5, 6];
    const TOKEN: Token = [1, 2, 3, 4, 5, 6, 7, 8];

    fn services(n: u32) -> Vec<Service> {
        (1..=n)
            .map(|t| Service::new(t, 0x31, vec![0xff; 3]).unwrap())
            .collect()
    }

    #[test]
    fn probe_round_trip() {
        let s = services(3);
        let f = probe(A, &TOKEN, &s, 9);
        let (h, kind) = classify(&f).unwrap().unwrap();
        assert_eq!((h.addr2, h.sequence()), (A, 9));
        let Kind::Probe(d) = kind else { panic!() };
        assert_eq!((d.token, d.services, d.problem), (Some(TOKEN), s, None));
    }

    #[test]
    fn services_split_and_fit() {
        let many = services(40);
        let f = probe(A, &TOKEN, &many, 0);
        let Some((_, Kind::Probe(d))) = classify(&f).unwrap() else {
            panic!()
        };
        assert_eq!(d.services.len(), DISCOVERY_BUDGET / 8);
        assert_eq!(d.services[..], many[..d.services.len()]);
        assert!(f.len() - HEADER_LEN < 2 + SSID.len() + 2 + 8 + 2 + 4 + 2 + 255);
    }

    #[test]
    fn offer_and_link_up() {
        let f = offer(A, B, &TOKEN, &services(1), 6, 1, 1000);
        let Some((h, Kind::Offer(d))) = classify(&f).unwrap() else {
            panic!()
        };
        assert_eq!((h.addr1, h.addr3, d.channel), (B, A, Some(6)));
        let f = link_up(A, B, 6, 2, 2000);
        assert!(matches!(classify(&f).unwrap(), Some((_, Kind::LinkUp))));
        assert_eq!(&f[f.len() - LINK_ELEMENTS.len()..], &LINK_ELEMENTS);
    }

    #[test]
    fn directed_probe_and_accept() {
        let f = directed_probe(B, A, 3);
        assert_eq!(&f[2..4], &DIRECTED_PROBE_DURATION.to_le_bytes());
        assert!(matches!(
            classify(&f).unwrap(),
            Some((_, Kind::DirectedProbe))
        ));
        let f = accept(B, A, 4);
        assert_eq!(&f[24..], &[0x7f, 0x00, 0x1f, 0x32, 0, 0]);
        assert!(matches!(classify(&f).unwrap(), Some((_, Kind::Accept))));
    }
}
