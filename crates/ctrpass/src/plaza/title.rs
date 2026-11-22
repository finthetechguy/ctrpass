use serde_json::Value;

use super::{
    MAX_BUBBLE, MAX_DREAM, MAX_HAT, MAX_HITS, MAX_HOBBY, MAX_PET, MAX_POPULATION, Names, Plaza,
    Profile, REFUSED_HATS, SEND_MODE, TITLE,
};
use crate::Error;
use crate::cec::icon::Icon;
use crate::mii::{NAME_LEN, RECORD_SIZE, Record};
use crate::title::{Context, Field, Get, Kind, Title, Values, hex};

/// Mii names may not hold these characters.
const NAME_FORBID: &str = "%\\";
/// Name new Miis and featured software are given.
const STANDARD_NAME: &str = "ctrpass";

const fn text(key: &'static str, label: &'static str, max: usize, help: &'static str) -> Field {
    Field {
        key,
        label,
        kind: Kind::Text { max, forbid: "" },
        help,
        advanced: false,
    }
}

const fn number(key: &'static str, label: &'static str, max: u64, help: &'static str) -> Field {
    Field {
        key,
        label,
        kind: Kind::Number {
            min: 0,
            max,
            exclude: &[],
            hex: false,
        },
        help,
        advanced: false,
    }
}

const fn hex_number(key: &'static str, label: &'static str, help: &'static str) -> Field {
    Field {
        key,
        label,
        kind: Kind::Number {
            min: 0,
            max: u32::MAX as u64,
            exclude: &[],
            hex: true,
        },
        help,
        advanced: true,
    }
}

const REFUSED: [u64; 4] = [
    REFUSED_HATS[0] as u64,
    REFUSED_HATS[1] as u64,
    REFUSED_HATS[2] as u64,
    REFUSED_HATS[3] as u64,
];

static FIELDS: &[Field] = &[
    Field {
        key: "name",
        label: "Mii name",
        kind: Kind::Text {
            max: NAME_LEN,
            forbid: NAME_FORBID,
        },
        help: "",
        advanced: false,
    },
    Field {
        key: "creator",
        label: "Creator",
        kind: Kind::Text {
            max: NAME_LEN,
            forbid: NAME_FORBID,
        },
        help: "Shown as who made the Mii",
        advanced: false,
    },
    text("greeting", "Greeting", 16, "Shown on the Mii's card"),
    number("hits", "StreetPass hits", MAX_HITS as u64, ""),
    number("population", "Plaza population", MAX_POPULATION as u64, ""),
    Field {
        key: "hat",
        label: "Hat",
        kind: Kind::Number {
            min: 0,
            max: MAX_HAT as u64,
            exclude: &REFUSED,
            hex: false,
        },
        help: "",
        advanced: false,
    },
    number("pet", "Pet", MAX_PET as u64, ""),
    number("dream", "Dream", MAX_DREAM as u64, ""),
    number("hobby", "Hobby", MAX_HOBBY as u64, ""),
    number("bubble", "Speech bubble", MAX_BUBBLE as u64, ""),
    text("country", "Country", 31, "Used for every language"),
    text("region", "Region", 31, "Used for every language"),
    Field {
        key: "featured",
        label: "Featured title ID",
        kind: Kind::Number {
            min: 1,
            max: u64::MAX - 1,
            exclude: &[],
            hex: true,
        },
        help: "The game shown on the card",
        advanced: false,
    },
    text(
        "short",
        "Featured short name",
        63,
        "Used for every language",
    ),
    text("long", "Featured long name", 127, "Used for every language"),
    text(
        "publisher",
        "Featured publisher",
        63,
        "Used for every language",
    ),
    Field {
        key: "icon",
        label: "Featured icon",
        kind: Kind::Image { size: 48 },
        help: "Also used for the small icon",
        advanced: false,
    },
    Field {
        key: "small_icon",
        label: "Featured small icon",
        kind: Kind::Image { size: 24 },
        help: "",
        advanced: true,
    },
    Field {
        key: "mii",
        label: "Mii record",
        kind: Kind::Bytes { len: RECORD_SIZE },
        help: "A Mii to start from",
        advanced: true,
    },
    hex_number("mii_id", "Mii ID", ""),
    Field {
        key: "creator_mac",
        label: "Creator MAC",
        kind: Kind::Bytes { len: 6 },
        help: "The console the Mii was made on",
        advanced: true,
    },
    hex_number("address", "Address code", "Country and region code"),
    hex_number("region_lockout", "Region lockout", ""),
    hex_number("presence", "Presence word", ""),
];

/// Scales a 48x48 icon to 24x24 by averaging each 2x2 block.
fn halve(icon: &Icon) -> Icon {
    let rgb = icon.to_rgb();
    let mut out = Vec::with_capacity(24 * 24);
    for y in 0..24 {
        for x in 0..24 {
            let mut sum = [0u32; 3];
            for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                let p = rgb[(2 * y + dy) * 48 + 2 * x + dx];
                for c in 0..3 {
                    sum[c] += u32::from(p[c]);
                }
            }
            out.push(sum.map(|s| (s / 4) as u8));
        }
    }
    Icon::from_rgb(24, 24, 24, &out)
}

pub struct PlazaTitle;

impl Title for PlazaTitle {
    fn id(&self) -> u32 {
        TITLE
    }

    fn name(&self) -> &'static str {
        "Mii Plaza"
    }

    fn send_mode(&self) -> u8 {
        SEND_MODE
    }

    fn fields(&self) -> &'static [Field] {
        FIELDS
    }

    fn create(&self, ctx: &Context) -> Result<Vec<u8>, Error> {
        let id = ctx
            .identity
            .ok_or_else(|| Error::Format("a new Plaza message needs an identity".into()))?;
        let mii = Record::new(
            STANDARD_NAME,
            id.mii_id,
            id.creator_mac,
            id.face_shape,
            id.gender,
        )?;
        let mut p = Plaza::new(ctx.record_key()?, &mii)?;
        p.set_profile(&Profile {
            hat: id.hat,
            dream: id.dream,
            hobby: id.hobby,
            ..p.profile()
        });
        p.set_names(&Names {
            short: STANDARD_NAME.into(),
            long: STANDARD_NAME.into(),
            publisher: STANDARD_NAME.into(),
        })?;
        Ok(p.as_bytes().to_vec())
    }

    fn read(&self, body: &[u8], ctx: &Context) -> Result<Values, Error> {
        let p = Plaza::parse(body)?;
        let mii = p.mii(ctx.record_key()?)?;
        let profile = p.profile();
        // The English slot stands for every language, since the fields set all of them alike.
        let names = p.names(1);
        let (country, region) = p.place(1);
        let (small, large) = p.icons();
        let mut v = Values::new();
        let mut put = |k: &str, x: Value| {
            v.insert(k.into(), x);
        };
        put("name", mii.name().into());
        put("creator", mii.creator_name().into());
        put("greeting", p.greeting().into());
        put("hits", profile.hits.into());
        put("population", profile.population.into());
        put("hat", profile.hat.into());
        put("pet", profile.pet.into());
        put("dream", profile.dream.into());
        put("hobby", profile.hobby.into());
        put("bubble", p.bubble().into());
        put("country", country.into());
        put("region", region.into());
        put("featured", p.featured_id().into());
        put("short", names.short.into());
        put("long", names.long.into());
        put("publisher", names.publisher.into());
        put("icon", hex(&large.encode()).into());
        put("small_icon", hex(&small.encode()).into());
        put("mii", hex(mii.as_bytes()).into());
        put("mii_id", mii.mii_id().into());
        put("creator_mac", hex(&mii.creator_mac()).into());
        put("address", p.address().into());
        put("region_lockout", p.region_lockout().into());
        put("presence", p.presence().into());
        Ok(v)
    }

    fn write(&self, body: &[u8], values: &Values, ctx: &Context) -> Result<Vec<u8>, Error> {
        let values = crate::title::check_all(self, values)?;
        let g = Get(&values);
        let mut p = Plaza::parse(body)?;
        let mii_keys = ["mii", "name", "creator", "mii_id", "creator_mac"];
        if mii_keys.iter().any(|k| values.contains_key(*k)) {
            let key = ctx.record_key()?;
            let mut mii = match g.bytes("mii") {
                Some(b) => Record::from_bytes(&b)?,
                None => p.mii(key)?,
            };
            if let Some(n) = g.text("name") {
                mii.set_name(n)?;
            }
            if let Some(n) = g.text("creator") {
                mii.set_creator_name(n)?;
            }
            let id = g.number("mii_id").map_or(mii.mii_id(), |n| n as u32);
            let mac = g
                .bytes("creator_mac")
                .map_or(mii.creator_mac(), |b| b.try_into().unwrap());
            mii.set_identity(id, mac);
            p.set_mii(key, &mii)?;
        }
        if let Some(s) = g.text("greeting") {
            p.set_greeting(s)?;
        }
        let mut prof = p.profile();
        if let Some(n) = g.number("hits") {
            prof.hits = n as u32;
        }
        if let Some(n) = g.number("population") {
            prof.population = n as u16;
        }
        for (key, slot) in [
            ("hat", &mut prof.hat),
            ("pet", &mut prof.pet),
            ("dream", &mut prof.dream),
            ("hobby", &mut prof.hobby),
        ] {
            if let Some(n) = g.number(key) {
                *slot = n as u8;
            }
        }
        p.set_profile(&prof);
        if let Some(n) = g.number("bubble") {
            p.set_bubble(n as u8);
        }
        if g.text("country").is_some() || g.text("region").is_some() {
            let (c, r) = p.place(1);
            p.set_place(
                g.text("country").unwrap_or(&c),
                g.text("region").unwrap_or(&r),
            )?;
        }
        if let Some(n) = g.number("featured") {
            p.set_featured_id(n);
        }
        if ["short", "long", "publisher"]
            .iter()
            .any(|k| values.contains_key(*k))
        {
            let old = p.names(1);
            p.set_names(&Names {
                short: g.text("short").map_or(old.short, str::to_string),
                long: g.text("long").map_or(old.long, str::to_string),
                publisher: g.text("publisher").map_or(old.publisher, str::to_string),
            })?;
        }
        let large = g.image("icon", 48);
        let small = g.image("small_icon", 24);
        if large.is_some() || small.is_some() {
            let (old_small, old_large) = p.icons();
            let large = large.unwrap_or(old_large);
            let small = small
                .or_else(|| g.image("icon", 48).map(|i| halve(&i)))
                .unwrap_or(old_small);
            p.set_icons(&small, &large)?;
        }
        if let Some(n) = g.number("address") {
            p.set_address(n as u32);
        }
        if let Some(n) = g.number("region_lockout") {
            p.set_region_lockout(n as u32);
        }
        if let Some(n) = g.number("presence") {
            p.set_presence(n as u32);
        }
        Ok(p.as_bytes().to_vec())
    }

    fn problems(&self, body: &[u8]) -> Vec<String> {
        match Plaza::parse(body) {
            Ok(p) => p.problems(),
            Err(e) => vec![e.to_string()],
        }
    }

    fn message_text(&self) -> &'static str {
        "A visitor arrived"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::Identity;
    use crate::title::{self, from_text};

    const KEY: crate::keys::Key = [0x31; 16];

    fn ctx(id: &Identity) -> Context<'_> {
        Context {
            record_key: Some(&KEY),
            identity: Some(id),
        }
    }

    #[test]
    fn create_read_write() {
        let id = Identity::from_seed(&[3; 32]);
        let t = title::find("Mii Plaza").unwrap();
        let body = t.create(&ctx(&id)).unwrap();
        let v = t.read(&body, &ctx(&id)).unwrap();
        assert_eq!(v["name"], "ctrpass");
        assert_eq!(v["mii_id"], u64::from(id.mii_id));
        assert_eq!(v["hat"], u64::from(id.hat));
        // Every value read back is accepted, and writing it all changes nothing.
        assert_eq!(t.write(&body, &v, &ctx(&id)).unwrap(), body);

        let mut edit = Values::new();
        for (k, text) in [
            ("greeting", "Hello"),
            ("name", "Ada"),
            ("hat", "12"),
            ("country", "Here"),
            ("featured", "0004000000123400"),
        ] {
            edit.insert(
                k.into(),
                from_text(title::field(t, k).unwrap(), text).unwrap(),
            );
        }
        edit.insert(
            "icon".into(),
            Value::from(hex(&Icon::filled(48, 0xf800).encode())),
        );
        let body2 = t.write(&body, &edit, &ctx(&id)).unwrap();
        let v2 = t.read(&body2, &ctx(&id)).unwrap();
        assert_eq!(
            (&v2["greeting"], &v2["name"], &v2["hat"], &v2["country"]),
            (&"Hello".into(), &"Ada".into(), &12.into(), &"Here".into())
        );
        assert_eq!(v2["featured"], 0x0004_0000_0012_3400u64);
        assert_eq!(v2["small_icon"], hex(&Icon::filled(24, 0xf800).encode()));
        assert_eq!(v2["region"], v["region"]);
        assert!(t.problems(&body2).is_empty());
    }

    #[test]
    fn refuses_bad_values() {
        let id = Identity::from_seed(&[4; 32]);
        let t = title::find("00020800").unwrap();
        let body = t.create(&ctx(&id)).unwrap();
        for (k, text) in [
            ("hat", "69"),
            ("name", "a%b"),
            ("greeting", "x".repeat(17).as_str()),
        ] {
            assert!(from_text(title::field(t, k).unwrap(), text).is_err(), "{k}");
        }
        let mut v = Values::new();
        v.insert("mii_id".into(), Value::from(0x20u64));
        assert!(t.write(&body, &v, &ctx(&id)).is_err());
        v.clear();
        v.insert("nope".into(), Value::from(1));
        assert!(t.write(&body, &v, &ctx(&id)).is_err());
        let no_key = Context {
            record_key: None,
            identity: Some(&id),
        };
        assert!(t.read(&body, &no_key).is_err());
    }
}
