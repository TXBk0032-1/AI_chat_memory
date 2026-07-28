use crate::{
    error::{AppError, Result},
    sync::bundle::ProtectionAlgorithm,
};
use argon2::{Algorithm, Argon2, Params, Version};
use chacha20poly1305::{
    KeyInit, XChaCha20Poly1305, XNonce,
    aead::{Aead, Payload},
};
use zeroize::Zeroizing;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Argon2idConfig {
    pub salt: [u8; 16],
    pub memory_kib: u32,
    pub iterations: u32,
    pub parallelism: u32,
}

pub trait PayloadProtector: Send + Sync {
    fn algorithm(&self) -> ProtectionAlgorithm;
    fn seal(&self, associated_data: &[u8], plaintext: &[u8], nonce: [u8; 24]) -> Result<Vec<u8>>;
    fn open(&self, associated_data: &[u8], ciphertext: &[u8], nonce: [u8; 24]) -> Result<Vec<u8>>;
}

pub struct XChaChaProtector {
    key: Zeroizing<[u8; 32]>,
}

impl XChaChaProtector {
    pub fn derive(passphrase: &str, config: &Argon2idConfig) -> Result<Self> {
        if passphrase.is_empty() {
            return Err(AppError::Crypto("passphrase is empty".into()));
        }
        let params = Params::new(
            config.memory_kib,
            config.iterations,
            config.parallelism,
            Some(32),
        )
        .map_err(|_| AppError::Crypto("invalid Argon2id parameters".into()))?;
        let argon2 = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
        let mut key = Zeroizing::new([0_u8; 32]);
        argon2
            .hash_password_into(passphrase.as_bytes(), &config.salt, key.as_mut())
            .map_err(|_| AppError::Crypto("Argon2id derivation failed".into()))?;
        Ok(Self { key })
    }

    fn cipher(&self) -> XChaCha20Poly1305 {
        XChaCha20Poly1305::new_from_slice(self.key.as_ref())
            .expect("XChaCha20-Poly1305 accepts a 32-byte key")
    }
}

impl PayloadProtector for XChaChaProtector {
    fn algorithm(&self) -> ProtectionAlgorithm {
        ProtectionAlgorithm::XChaCha20Poly1305
    }

    fn seal(&self, associated_data: &[u8], plaintext: &[u8], nonce: [u8; 24]) -> Result<Vec<u8>> {
        self.cipher()
            .encrypt(
                XNonce::from_slice(&nonce),
                Payload {
                    msg: plaintext,
                    aad: associated_data,
                },
            )
            .map_err(|_| AppError::Crypto("payload encryption failed".into()))
    }

    fn open(&self, associated_data: &[u8], ciphertext: &[u8], nonce: [u8; 24]) -> Result<Vec<u8>> {
        self.cipher()
            .decrypt(
                XNonce::from_slice(&nonce),
                Payload {
                    msg: ciphertext,
                    aad: associated_data,
                },
            )
            .map_err(|_| AppError::Crypto("payload authentication failed".into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_kdf() -> Argon2idConfig {
        Argon2idConfig {
            salt: [3; 16],
            memory_kib: 8 * 1024,
            iterations: 2,
            parallelism: 1,
        }
    }

    #[test]
    fn encrypted_payload_has_stable_vector_and_authenticates_header() {
        let protector = XChaChaProtector::derive("correct horse", &test_kdf()).unwrap();
        let nonce = [7_u8; 24];
        let sealed = protector.seal(b"header", b"conversation", nonce).unwrap();

        assert_eq!(
            hex::encode(&sealed),
            "d0d685656c3dec427f4f0234eaee2a0dd3e3149c5622de8ffdf6af7a"
        );
        assert_eq!(
            protector.open(b"header", &sealed, nonce).unwrap(),
            b"conversation"
        );
        assert!(protector.open(b"changed", &sealed, nonce).is_err());
        assert!(
            XChaChaProtector::derive("wrong", &test_kdf())
                .unwrap()
                .open(b"header", &sealed, nonce)
                .is_err()
        );
    }

    #[test]
    fn encrypted_payload_rejects_ciphertext_and_nonce_changes() {
        let protector = XChaChaProtector::derive("correct horse", &test_kdf()).unwrap();
        let nonce = [9_u8; 24];
        let mut sealed = protector.seal(b"header", b"payload", nonce).unwrap();
        sealed[0] ^= 1;
        assert!(protector.open(b"header", &sealed, nonce).is_err());
        assert!(protector.open(b"header", &sealed, [8_u8; 24]).is_err());
    }
}
