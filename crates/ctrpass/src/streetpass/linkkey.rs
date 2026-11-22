//! The CCMP key two consoles share once they have found each other.

use aes::Aes128;
use aes::cipher::{BlockEncrypt, KeyInit};
use hmac::{Hmac, Mac as _};
use sha1::Sha1;

use crate::keys::{Key, Keys};
use crate::wifi::frame::Mac;

/// The 8-byte value each console sends in its discovery frames.
pub type Token = [u8; 8];

/// Derives the link key, with the side that hosts the link given first.
pub fn derive(keys: &Keys, host: (&Token, &Mac), client: (&Token, &Mac)) -> Key {
    let mut mac =
        <Hmac<Sha1> as hmac::Mac>::new_from_slice(&keys.hmac).expect("HMAC takes any key length");
    mac.update(host.0);
    mac.update(client.0);
    mac.update(host.1);
    mac.update(client.1);
    let digest = mac.finalize().into_bytes();
    let mut block = aes::Block::clone_from_slice(&digest[..16]);
    Aes128::new(&keys.link.into()).encrypt_block(&mut block);
    block.into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_vector() {
        let text = "slot0x2EKeyN=000102030405060708090a0b0c0d0e0f\n\
                    streetpassHmacKey=41414141414141414141414141414141\n";
        let keys = Keys::parse(text).unwrap();
        let host_token: Token = [0, 1, 2, 3, 4, 5, 6, 7];
        let client_token: Token = [8, 9, 10, 11, 12, 13, 14, 15];
        let host_mac: Mac = [0xba, 0xae, 0x6e, 1, 2, 3];
        let client_mac: Mac = [0x94, 0x58, 0x6a, 0x0b, 0x0c, 0x0d];
        let key = derive(
            &keys,
            (&host_token, &host_mac),
            (&client_token, &client_mac),
        );
        assert_eq!(
            key,
            [
                0xbb, 0x43, 0xe7, 0x2f, 0x7a, 0xb3, 0x1b, 0x37, 0xa8, 0xf6, 0x8c, 0xbc, 0x15, 0xe1,
                0x71, 0x51
            ]
        );
        assert_ne!(
            key,
            derive(
                &keys,
                (&client_token, &client_mac),
                (&host_token, &host_mac)
            )
        );
    }
}
