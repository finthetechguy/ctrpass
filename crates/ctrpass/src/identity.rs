//! The console identity a store presents, derived from its 32-byte seed.

use crate::wifi::frame::Mac;

pub type Seed = [u8; 32];

/// Hat IDs the Plaza refuses.
const REFUSED_HATS: [u8; 4] = [69, 75, 168, 169];
const HATS: u8 = 76;
const FACE_SHAPES: u8 = 12;
const DREAMS: u8 = 20;
const HOBBIES: u8 = 27;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Identity {
    /// Locally administered address the radio transmits from.
    pub mac: Mac,
    /// Address recorded as the Mii's creator, shaped like a factory address.
    pub creator_mac: Mac,
    pub mii_id: u32,
    pub face_shape: u8,
    pub hat: u8,
    pub dream: u8,
    pub hobby: u8,
    pub gender: u8,
}

pub fn random_seed() -> std::io::Result<Seed> {
    let mut seed = [0u8; 32];
    getrandom::fill(&mut seed).map_err(|e| std::io::Error::other(e.to_string()))?;
    Ok(seed)
}

impl Identity {
    pub fn from_seed(seed: &Seed) -> Self {
        let mut mac: Mac = seed[0..6].try_into().unwrap();
        mac[0] = (mac[0] & 0xfc) | 0x02;
        let mut creator_mac: Mac = seed[6..12].try_into().unwrap();
        creator_mac[0] &= 0xfc;
        // The Plaza drops a Mii whose ID has bit 5 set, and console-made IDs start with 0x9.
        let mii_id = (0x90 | u32::from(seed[12] & 0x0f))
            | u32::from_le_bytes([seed[13], seed[14], seed[15], 0]) << 8;
        let hat = seed[16] % HATS;
        Identity {
            mac,
            creator_mac,
            mii_id,
            face_shape: seed[17] % FACE_SHAPES,
            hat: if REFUSED_HATS.contains(&hat) { 0 } else { hat },
            dream: seed[18] % DREAMS,
            hobby: seed[19] % HOBBIES,
            gender: seed[20] & 1,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fields_stay_valid() {
        for i in 0..=255u8 {
            let seed = [i; 32];
            let id = Identity::from_seed(&seed);
            assert_eq!(id.mac[0] & 3, 2);
            assert_eq!(id.creator_mac[0] & 3, 0);
            assert_eq!(id.mii_id & 0xf0, 0x90);
            assert!(!REFUSED_HATS.contains(&id.hat));
            assert!(id.face_shape < FACE_SHAPES && id.dream < DREAMS && id.hobby < HOBBIES);
            assert_eq!(id, Identity::from_seed(&seed));
        }
    }

    #[test]
    fn seeds_differ() {
        let a = Identity::from_seed(&random_seed().unwrap());
        let b = Identity::from_seed(&random_seed().unwrap());
        assert_ne!((a.mac, a.mii_id), (b.mac, b.mii_id));
    }
}
