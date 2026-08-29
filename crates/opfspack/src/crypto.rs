//! Crypto primitives for identity-bound packs. Constants and derivation
//! order match the TS `auth/identity-key.ts` exactly.

use aes_gcm::{Aes256Gcm, KeyInit, Nonce, aead::Aead};
use hkdf::Hkdf;
use pbkdf2::pbkdf2_hmac;
use rand::RngCore;
use sha2::Sha256;

pub(crate) const APP_SALT: &[u8] = b"opfspack-v1-identity-salt-2024";
pub(crate) const PBKDF2_ITERATIONS: u32 = 100_000;
pub(crate) const HKDF_INFO: &[u8] = b"opfspack-entry-key";

/// IEEE CRC-32 (same as the TS reference table implementation).
pub(crate) fn crc32(bytes: &[u8]) -> u32 {
    crc32fast::hash(bytes)
}

/// masterKey = PBKDF2-SHA256(password = sub + APP_SALT, salt = APP_SALT,
/// 100_000 iterations, 32 bytes).
pub(crate) fn master_key(sub: &str) -> [u8; 32] {
    let mut password = sub.as_bytes().to_vec();
    password.extend_from_slice(APP_SALT);
    let mut key = [0u8; 32];
    pbkdf2_hmac::<Sha256>(&password, APP_SALT, PBKDF2_ITERATIONS, &mut key);
    key
}

/// packKey = HKDF-SHA256(ikm = masterKey, salt = packId,
/// info = "opfspack-entry-key", 32 bytes).
pub(crate) fn pack_key(master: &[u8; 32], pack_id: &str) -> [u8; 32] {
    let hk = Hkdf::<Sha256>::new(Some(pack_id.as_bytes()), master);
    let mut key = [0u8; 32];
    hk.expand(HKDF_INFO, &mut key)
        .expect("32-byte HKDF output is valid");
    key
}

/// AES-256-GCM encrypt; returns (random 12-byte IV, ciphertext || tag).
pub(crate) fn encrypt(data: &[u8], key: &[u8; 32]) -> ([u8; 12], Vec<u8>) {
    let cipher = Aes256Gcm::new_from_slice(key).expect("32-byte key is valid for AES-256");
    let mut iv = [0u8; 12];
    rand::rngs::OsRng.fill_bytes(&mut iv);
    let ciphertext = cipher
        .encrypt(Nonce::from_slice(&iv), data)
        .expect("in-memory AES-GCM encryption cannot fail");
    (iv, ciphertext)
}

/// AES-256-GCM decrypt; `Err(())` on tag/authentication failure.
pub(crate) fn decrypt(data: &[u8], iv: &[u8; 12], key: &[u8; 32]) -> Result<Vec<u8>, ()> {
    let cipher = Aes256Gcm::new_from_slice(key).expect("32-byte key is valid for AES-256");
    cipher.decrypt(Nonce::from_slice(iv), data).map_err(|_| ())
}
