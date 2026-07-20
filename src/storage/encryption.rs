use aes_gcm::{
    Aes256Gcm, Nonce,
    aead::{Aead, AeadCore, KeyInit, OsRng, Payload},
};
use anyhow::{Context, Result};
use std::sync::Arc;
use zeroize::Zeroizing;

const ENVELOPE_MAGIC: &[u8; 4] = b"DAE1";
const NONCE_BYTES: usize = 12;

#[derive(Clone)]
pub(crate) struct RecordCipher {
    cipher: Arc<Aes256Gcm>,
}

impl RecordCipher {
    pub(crate) fn new(key: [u8; 32]) -> Result<Self> {
        let key = Zeroizing::new(key);
        let cipher = Aes256Gcm::new_from_slice(key.as_ref())
            .map_err(|_| anyhow::anyhow!("invalid database encryption key length"))?;
        Ok(Self {
            cipher: Arc::new(cipher),
        })
    }

    pub(crate) fn is_encrypted(value: &[u8]) -> bool {
        value.starts_with(ENVELOPE_MAGIC)
    }

    pub(crate) fn seal(&self, domain: &[u8], record_key: &[u8], value: &[u8]) -> Result<Vec<u8>> {
        let nonce = Aes256Gcm::generate_nonce(&mut OsRng);
        let aad = associated_data(domain, record_key);
        let ciphertext = self
            .cipher
            .encrypt(
                &nonce,
                Payload {
                    msg: value,
                    aad: &aad,
                },
            )
            .map_err(|_| anyhow::anyhow!("failed to encrypt sensitive database record"))?;
        let mut envelope =
            Vec::with_capacity(ENVELOPE_MAGIC.len() + NONCE_BYTES + ciphertext.len());
        envelope.extend_from_slice(ENVELOPE_MAGIC);
        envelope.extend_from_slice(&nonce);
        envelope.extend_from_slice(&ciphertext);
        Ok(envelope)
    }

    pub(crate) fn open(&self, domain: &[u8], record_key: &[u8], value: &[u8]) -> Result<Vec<u8>> {
        if !Self::is_encrypted(value) {
            return Ok(value.to_vec());
        }
        anyhow::ensure!(
            value.len() >= ENVELOPE_MAGIC.len() + NONCE_BYTES + 16,
            "encrypted database record is truncated"
        );
        let nonce_start = ENVELOPE_MAGIC.len();
        let ciphertext_start = nonce_start + NONCE_BYTES;
        let nonce = Nonce::from_slice(&value[nonce_start..ciphertext_start]);
        let aad = associated_data(domain, record_key);
        self.cipher
            .decrypt(
                nonce,
                Payload {
                    msg: &value[ciphertext_start..],
                    aad: &aad,
                },
            )
            .map_err(|_| anyhow::anyhow!("failed to decrypt sensitive database record"))
            .with_context(
                || "DATA_ENCRYPTION_KEY does not match the database or the record is corrupted",
            )
    }
}

fn associated_data(domain: &[u8], record_key: &[u8]) -> Vec<u8> {
    let mut aad = Vec::with_capacity(38 + domain.len() + record_key.len());
    aad.extend_from_slice(b"disaster-alert:encrypted-record:v1\0");
    aad.extend_from_slice(domain);
    aad.push(0);
    aad.extend_from_slice(record_key);
    aad
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encryption_is_randomized_and_bound_to_its_record() -> Result<()> {
        let cipher = RecordCipher::new([7; 32])?;
        let first = cipher.seal(b"subscriptions", b"1", b"secret")?;
        let second = cipher.seal(b"subscriptions", b"1", b"secret")?;

        anyhow::ensure!(first != second);
        anyhow::ensure!(cipher.open(b"subscriptions", b"1", &first)? == b"secret");
        anyhow::ensure!(cipher.open(b"subscriptions", b"2", &first).is_err());
        Ok(())
    }

    #[test]
    fn legacy_plaintext_is_returned_for_startup_migration() -> Result<()> {
        let cipher = RecordCipher::new([7; 32])?;
        anyhow::ensure!(cipher.open(b"subscriptions", b"1", b"legacy")? == b"legacy");
        Ok(())
    }
}
