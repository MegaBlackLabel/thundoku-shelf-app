//! Crypto primitives for v3 packs: PBKDF2 / HKDF derivation, AES-256-GCM
//! (with AAD for the root-key wraps), and CRC-32. Constants and derivation
//! order match the TS implementation (`auth/identity-key.ts` and the
//! `docs/spec/10-pack-keys.md` §2 equivalents).

use aes_gcm::{
    Aes256Gcm, KeyInit, Nonce,
    aead::{Aead, Payload},
};
use hkdf::Hkdf;
use pbkdf2::pbkdf2_hmac;
use rand::RngCore;
use sha2::Sha256;

/// HKDF info string for the per-pack entry key (v2 と同一）。
pub(crate) const HKDF_INFO: &[u8] = b"opfspack-entry-key";

/// IEEE CRC-32 (same as the TS reference table implementation).
pub(crate) fn crc32(bytes: &[u8]) -> u32 {
    crc32fast::hash(bytes)
}

/// PBKDF2-HMAC-SHA256 → 32 bytes.
pub(crate) fn pbkdf2_sha256(password: &[u8], salt: &[u8], iterations: u32) -> [u8; 32] {
    let mut key = [0u8; 32];
    pbkdf2_hmac::<Sha256>(password, salt, iterations, &mut key);
    key
}

/// HKDF-SHA256(ikm, salt, info) → 32 bytes.
pub(crate) fn hkdf_sha256(ikm: &[u8], salt: &[u8], info: &[u8]) -> [u8; 32] {
    let hk = Hkdf::<Sha256>::new(Some(salt), ikm);
    let mut key = [0u8; 32];
    hk.expand(info, &mut key)
        .expect("32-byte HKDF output is valid");
    key
}

/// AES-256-GCM encrypt with a random 12-byte IV; returns (IV, ciphertext || tag).
pub(crate) fn encrypt(data: &[u8], key: &[u8; 32]) -> ([u8; 12], Vec<u8>) {
    let mut iv = [0u8; 12];
    rand::rngs::OsRng.fill_bytes(&mut iv);
    (iv, encrypt_with(data, key, &iv, &[]))
}

/// AES-256-GCM encrypt with a caller-supplied IV and AAD.
pub(crate) fn encrypt_with(data: &[u8], key: &[u8; 32], iv: &[u8; 12], aad: &[u8]) -> Vec<u8> {
    let cipher = Aes256Gcm::new_from_slice(key).expect("32-byte key is valid for AES-256");
    cipher
        .encrypt(Nonce::from_slice(iv), Payload { msg: data, aad })
        .expect("in-memory AES-GCM encryption cannot fail")
}

/// AES-256-GCM decrypt without AAD; `Err(())` on tag/authentication failure.
pub(crate) fn decrypt(data: &[u8], iv: &[u8; 12], key: &[u8; 32]) -> Result<Vec<u8>, ()> {
    decrypt_with(data, iv, key, &[])
}

/// AES-256-GCM decrypt with AAD; `Err(())` on tag/authentication failure.
pub(crate) fn decrypt_with(
    data: &[u8],
    iv: &[u8; 12],
    key: &[u8; 32],
    aad: &[u8],
) -> Result<Vec<u8>, ()> {
    let cipher = Aes256Gcm::new_from_slice(key).expect("32-byte key is valid for AES-256");
    cipher
        .decrypt(Nonce::from_slice(iv), Payload { msg: data, aad })
        .map_err(|_| ())
}
