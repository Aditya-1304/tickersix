//! Encrypted lineup-recovery primitives for the automatic reveal boundary.
//!
//! The on-chain commitment remains authoritative. This module only protects a
//! recovery copy until the reveal worker can submit the already-committed
//! preimage. It intentionally does not hold player private keys or submit
//! transactions itself.

use std::fmt;

use aes_gcm::{
    aead::{Aead, AeadCore, KeyInit, OsRng, Payload},
    Aes256Gcm, Nonce,
};

pub const RECOVERY_PAYLOAD_VERSION: u16 = 1;
pub const RECOVERY_NONCE_BYTES: usize = 12;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecoveryError {
    InvalidLineup,
    CommitmentMismatch,
    InvalidCiphertext,
    KeyVersionMismatch,
}

impl fmt::Display for RecoveryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidLineup => "lineup is invalid",
            Self::CommitmentMismatch => "recovery payload does not match commitment",
            Self::InvalidCiphertext => "encrypted recovery payload is invalid",
            Self::KeyVersionMismatch => "recovery key version does not match ciphertext",
        })
    }
}

impl std::error::Error for RecoveryError {}

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct LineupRecovery {
    pub asset_ids: [u16; protocol::LINEUP_SIZE],
    pub captain_asset_id: u16,
    pub salt: [u8; 32],
}

impl LineupRecovery {
    pub fn new(
        mut asset_ids: [u16; protocol::LINEUP_SIZE],
        captain_asset_id: u16,
        salt: [u8; 32],
    ) -> Result<Self, RecoveryError> {
        protocol::validate_lineup(asset_ids, captain_asset_id)
            .map_err(|_| RecoveryError::InvalidLineup)?;
        if asset_ids.iter().any(|asset_id| *asset_id >= 256) {
            return Err(RecoveryError::InvalidLineup);
        }
        asset_ids.sort_unstable();
        Ok(Self {
            asset_ids,
            captain_asset_id,
            salt,
        })
    }

    fn encode(self) -> [u8; 48] {
        let mut bytes = [0u8; 48];
        bytes[..2].copy_from_slice(&RECOVERY_PAYLOAD_VERSION.to_le_bytes());
        for (index, asset_id) in self.asset_ids.into_iter().enumerate() {
            let start = 2 + index * 2;
            bytes[start..start + 2].copy_from_slice(&asset_id.to_le_bytes());
        }
        bytes[14..16].copy_from_slice(&self.captain_asset_id.to_le_bytes());
        bytes[16..].copy_from_slice(&self.salt);
        bytes
    }

    fn decode(bytes: &[u8]) -> Result<Self, RecoveryError> {
        if bytes.len() != 48 || u16::from_le_bytes([bytes[0], bytes[1]]) != RECOVERY_PAYLOAD_VERSION
        {
            return Err(RecoveryError::InvalidCiphertext);
        }

        let mut asset_ids = [0u16; protocol::LINEUP_SIZE];
        for (index, asset_id) in asset_ids.iter_mut().enumerate() {
            let start = 2 + index * 2;
            *asset_id = u16::from_le_bytes([bytes[start], bytes[start + 1]]);
        }
        let captain_asset_id = u16::from_le_bytes([bytes[14], bytes[15]]);
        let mut salt = [0u8; 32];
        salt.copy_from_slice(&bytes[16..]);
        Self::new(asset_ids, captain_asset_id, salt).map_err(|_| RecoveryError::InvalidCiphertext)
    }
}

impl fmt::Debug for LineupRecovery {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LineupRecovery")
            .field("asset_ids", &self.asset_ids)
            .field("captain_asset_id", &self.captain_asset_id)
            .field("salt", &"<redacted>")
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RecoveryContext {
    pub program_id: [u8; 32],
    pub battle_pubkey: [u8; 32],
    pub player_pubkey: [u8; 32],
    pub registry_version: u32,
    pub commitment: [u8; 32],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncryptedLineupRecovery {
    pub key_version: u32,
    pub nonce: [u8; RECOVERY_NONCE_BYTES],
    pub ciphertext: Vec<u8>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct RecoveryKey {
    pub version: u32,
    key: [u8; 32],
}

impl RecoveryKey {
    pub fn new(version: u32, key: [u8; 32]) -> Self {
        Self { version, key }
    }

    pub fn encrypt(
        &self,
        context: &RecoveryContext,
        payload: &LineupRecovery,
        nonce: [u8; RECOVERY_NONCE_BYTES],
    ) -> Result<EncryptedLineupRecovery, RecoveryError> {
        let expected_commitment = protocol::commitment(
            context.program_id,
            context.battle_pubkey,
            context.player_pubkey,
            context.registry_version,
            payload.asset_ids,
            payload.captain_asset_id,
            payload.salt,
        );
        if expected_commitment != context.commitment {
            return Err(RecoveryError::CommitmentMismatch);
        }
        let cipher =
            Aes256Gcm::new_from_slice(&self.key).map_err(|_| RecoveryError::InvalidCiphertext)?;
        let aad = context.associated_data();
        let ciphertext = cipher
            .encrypt(
                Nonce::from_slice(&nonce),
                Payload {
                    msg: &payload.encode(),
                    aad: &aad,
                },
            )
            .map_err(|_| RecoveryError::InvalidCiphertext)?;
        Ok(EncryptedLineupRecovery {
            key_version: self.version,
            nonce,
            ciphertext,
        })
    }

    /// Encrypts a recovery payload with a cryptographically fresh nonce.
    ///
    /// Callers that need deterministic test vectors should use `encrypt` and
    /// supply a fixed nonce explicitly; production callers should use this
    /// method and persist the returned nonce with the ciphertext.
    pub fn encrypt_random(
        &self,
        context: &RecoveryContext,
        payload: &LineupRecovery,
    ) -> Result<EncryptedLineupRecovery, RecoveryError> {
        let nonce = Aes256Gcm::generate_nonce(&mut OsRng);
        self.encrypt(context, payload, nonce.into())
    }

    pub fn decrypt(
        &self,
        context: &RecoveryContext,
        encrypted: &EncryptedLineupRecovery,
    ) -> Result<LineupRecovery, RecoveryError> {
        if encrypted.key_version != self.version {
            return Err(RecoveryError::KeyVersionMismatch);
        }
        let cipher =
            Aes256Gcm::new_from_slice(&self.key).map_err(|_| RecoveryError::InvalidCiphertext)?;
        let aad = context.associated_data();
        let plaintext = cipher
            .decrypt(
                Nonce::from_slice(&encrypted.nonce),
                Payload {
                    msg: &encrypted.ciphertext,
                    aad: &aad,
                },
            )
            .map_err(|_| RecoveryError::InvalidCiphertext)?;
        LineupRecovery::decode(&plaintext)
    }
}

impl RecoveryContext {
    fn associated_data(self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(128);
        bytes.extend_from_slice(b"TICKERSIX_LINEUP_RECOVERY_V1\0");
        bytes.extend_from_slice(&self.program_id);
        bytes.extend_from_slice(&self.battle_pubkey);
        bytes.extend_from_slice(&self.player_pubkey);
        bytes.extend_from_slice(&self.registry_version.to_le_bytes());
        bytes.extend_from_slice(&self.commitment);
        bytes
    }
}

impl fmt::Debug for RecoveryKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RecoveryKey")
            .field("version", &self.version)
            .field("key", &"<redacted>")
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encrypted_recovery_round_trip_is_bound_to_context_and_key_version() {
        // Regression target: a recovery ciphertext must not be transplantable
        // to another battle/player or silently decrypted with a rotated key.
        let key = RecoveryKey::new(7, [3; 32]);
        let context = RecoveryContext {
            program_id: [8; 32],
            battle_pubkey: [1; 32],
            player_pubkey: [2; 32],
            registry_version: 1,
            commitment: protocol::commitment(
                [8; 32],
                [1; 32],
                [2; 32],
                1,
                [9, 1, 4, 2, 8, 3],
                4,
                [5; 32],
            ),
        };
        let payload = LineupRecovery::new([9, 1, 4, 2, 8, 3], 4, [5; 32]).unwrap();
        let encrypted = key
            .encrypt(&context, &payload, [6; RECOVERY_NONCE_BYTES])
            .unwrap();

        assert_eq!(key.decrypt(&context, &encrypted).unwrap(), payload);

        let different_context = RecoveryContext {
            battle_pubkey: [9; 32],
            ..context
        };
        assert_eq!(
            key.decrypt(&different_context, &encrypted),
            Err(RecoveryError::InvalidCiphertext)
        );

        let rotated_key = RecoveryKey::new(8, [3; 32]);
        assert_eq!(
            rotated_key.decrypt(&context, &encrypted),
            Err(RecoveryError::KeyVersionMismatch)
        );

        let mismatched_commitment = RecoveryContext {
            commitment: [99; 32],
            ..context
        };
        assert_eq!(
            key.encrypt(&mismatched_commitment, &payload, [6; RECOVERY_NONCE_BYTES]),
            Err(RecoveryError::CommitmentMismatch)
        );
    }
}
