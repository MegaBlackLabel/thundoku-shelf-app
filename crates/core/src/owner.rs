//! Owner-sub encryption for `books.owner_sub`.
//!
//! The pack key derives from `sub + pack_id` (`PBKDF2 -> HKDF`), so storing the
//! plaintext `sub` in the DB (which already holds `pack_id` in the clear) would
//! let anyone with the DB derive every pack key. Therefore the owner sub is
//! stored as an AES-256-GCM ciphertext under a keyring-held key (P1), and only
//! ever compared in memory after decryption (P2).

use aes_gcm::{Aes256Gcm, KeyInit, Nonce, aead::Aead};
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use rand::RngCore;

/// Encrypt a `sub` into a BASE64 `IV(12) || ciphertext || tag` blob.
pub fn encrypt(key: &[u8; 32], sub: &str) -> String {
    let cipher = Aes256Gcm::new_from_slice(key).expect("32-byte AES-256 key is valid");
    let mut iv = [0u8; 12];
    rand::rngs::OsRng.fill_bytes(&mut iv);
    let ct = cipher
        .encrypt(Nonce::from_slice(&iv), sub.as_bytes())
        .expect("in-memory AES-256-GCM encryption cannot fail");
    let mut out = Vec::with_capacity(12 + ct.len());
    out.extend_from_slice(&iv);
    out.extend_from_slice(&ct);
    B64.encode(&out)
}

/// Decrypt a blob back to the `sub`. `None` on wrong key / corruption /
/// non-UTF-8 payload (P1: unknown owner is not treated as 未所属, it is hidden).
pub fn decrypt(key: &[u8; 32], blob: &str) -> Option<String> {
    let data = B64.decode(blob).ok()?;
    if data.len() < 12 {
        return None;
    }
    let (iv, ct) = data.split_at(12);
    let cipher = Aes256Gcm::new_from_slice(key).ok()?;
    let pt = cipher.decrypt(Nonce::from_slice(iv), ct).ok()?;
    String::from_utf8(pt).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: [u8; 32] = [7u8; 32];

    #[test]
    fn roundtrip() {
        let blob = encrypt(&KEY, "some-sub-123");
        assert_eq!(decrypt(&KEY, &blob).as_deref(), Some("some-sub-123"));
    }

    #[test]
    fn wrong_key_is_none() {
        let blob = encrypt(&KEY, "secret");
        let wrong = [9u8; 32];
        assert!(decrypt(&wrong, &blob).is_none());
    }

    #[test]
    fn non_utf8_payload_returns_none() {
        // Build a valid blob with non-UTF-8 plaintext.
        let cipher = Aes256Gcm::new_from_slice(&KEY).unwrap();
        let mut iv = [0u8; 12];
        rand::rngs::OsRng.fill_bytes(&mut iv);
        let ct = cipher
            .encrypt(Nonce::from_slice(&iv), &[0xffu8, 0xfeu8, 0x00u8][..])
            .unwrap();
        let mut out = Vec::with_capacity(12 + ct.len());
        out.extend_from_slice(&iv);
        out.extend_from_slice(&ct);
        let blob = B64.encode(&out);
        assert!(decrypt(&KEY, &blob).is_none());
    }

    #[test]
    fn gibberish_returns_none() {
        assert!(decrypt(&KEY, "not-a-valid-blob").is_none());
    }
}
