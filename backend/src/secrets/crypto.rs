//! Per-version envelope encryption. Immutable opaque identifiers bind both
//! layers; names and ACLs may change without invalidating ciphertext.
use openssl::{
    rand::rand_bytes,
    symm::{decrypt_aead, encrypt_aead, Cipher},
};
use std::{fmt, path::Path, sync::Arc};
use uuid::Uuid;
use zeroize::{Zeroize, Zeroizing};

pub struct SensitiveValue(pub Zeroizing<Vec<u8>>);
impl fmt::Debug for SensitiveValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("[REDACTED]")
    }
}
impl fmt::Display for SensitiveValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("[REDACTED]")
    }
}
impl SensitiveValue {
    pub fn new(bytes: Vec<u8>) -> Self {
        Self(Zeroizing::new(bytes))
    }
    pub fn bytes(&self) -> &[u8] {
        &self.0
    }
}
#[derive(Debug, Clone, Copy)]
pub struct Identity {
    pub organization: Uuid,
    pub secret: Uuid,
    pub version: Uuid,
}
impl Identity {
    fn aad(self) -> Vec<u8> {
        format!(
            "agent-portal-vault:1:{}:{}:{}",
            self.organization, self.secret, self.version
        )
        .into_bytes()
    }
}
/// The master key exists only in the injected file and process memory. Never
/// derive it from database, cookie or JWT credentials, or generate a fallback.
#[derive(Clone)]
pub struct EnvelopeCrypto {
    pub(crate) key: Arc<Zeroizing<Vec<u8>>>,
}
#[derive(Debug, Clone)]
pub struct Envelope {
    pub ciphertext: Vec<u8>,
    pub nonce: Vec<u8>,
    pub tag: Vec<u8>,
    pub wrapped_dek: Vec<u8>,
    pub wrapping_key_id: String,
    pub crypto_format: i32,
}
#[derive(Debug, thiserror::Error)]
#[error("Secret encryption service unavailable or ciphertext authentication failed")]
pub struct CryptoError;
impl EnvelopeCrypto {
    pub fn from_env() -> anyhow::Result<Option<Self>> {
        match std::env::var_os("PORTAL_SECRETS_KEY_FILE") {
            None => Ok(None),
            Some(path) if path.is_empty() => anyhow::bail!("Secrets key file path is empty"),
            Some(path) => Ok(Some(Self::key_file(Path::new(&path))?)),
        }
    }
    /// Open the injected key without following symlinks or blocking on a FIFO.
    /// Validate the opened descriptor, avoiding path/permission-check races.
    pub fn key_file(path: &Path) -> anyhow::Result<Self> {
        #[cfg(not(unix))]
        {
            let _ = path;
            anyhow::bail!("Secrets key files require Unix permission checks");
        }
        #[cfg(unix)]
        {
            use std::{
                io::Read,
                os::unix::fs::{MetadataExt, OpenOptionsExt},
            };
            let file = std::fs::OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
                .open(path)?;
            let metadata = file.metadata()?;
            if !metadata.is_file()
                || metadata.mode() & 0o077 != 0
                || metadata.uid() != unsafe { libc::geteuid() }
            {
                anyhow::bail!("Secrets key must be a regular file owned by the backend user with mode 0400 or 0600");
            }
            let mut key = Zeroizing::new(Vec::with_capacity(33));
            file.take(33).read_to_end(&mut key)?;
            if key.len() != 32 {
                anyhow::bail!("Secrets key file must contain exactly 32 raw bytes");
            }
            Ok(Self { key: Arc::new(key) })
        }
    }
    pub async fn seal(
        &self,
        id: Identity,
        value: &SensitiveValue,
    ) -> Result<Envelope, CryptoError> {
        if value.bytes().is_empty() || value.bytes().len() > 65536 {
            return Err(CryptoError);
        }
        let aad = id.aad();
        let mut dek = Zeroizing::new(vec![0; 32]);
        rand_bytes(&mut dek).map_err(|_| CryptoError)?;
        let mut wrap_nonce = vec![0; 12];
        rand_bytes(&mut wrap_nonce).map_err(|_| CryptoError)?;
        let mut wrap_tag = vec![0; 16];
        let encrypted_dek = encrypt_aead(
            Cipher::aes_256_gcm(),
            &self.key,
            Some(&wrap_nonce),
            &aad,
            &dek,
            &mut wrap_tag,
        )
        .map_err(|_| CryptoError)?;
        let mut wrapped_dek = wrap_nonce;
        wrapped_dek.extend(wrap_tag);
        wrapped_dek.extend(encrypted_dek);
        let wrapping_key_id = "key-file-v1".into();
        if dek.len() != 32 {
            return Err(CryptoError);
        }
        let mut nonce = vec![0; 12];
        rand_bytes(&mut nonce).map_err(|_| CryptoError)?;
        let mut tag = vec![0; 16];
        let ciphertext = encrypt_aead(
            Cipher::aes_256_gcm(),
            &dek,
            Some(&nonce),
            &aad,
            value.bytes(),
            &mut tag,
        )
        .map_err(|_| CryptoError)?;
        dek.zeroize();
        Ok(Envelope {
            ciphertext,
            nonce,
            tag,
            wrapped_dek,
            wrapping_key_id,
            crypto_format: 1,
        })
    }
    pub async fn open(
        &self,
        id: Identity,
        envelope: &Envelope,
    ) -> Result<SensitiveValue, CryptoError> {
        if envelope.crypto_format != 1 || envelope.nonce.len() != 12 || envelope.tag.len() != 16 {
            return Err(CryptoError);
        }
        let aad = id.aad();
        if envelope.wrapping_key_id != "key-file-v1" || envelope.wrapped_dek.len() != 60 {
            return Err(CryptoError);
        }
        let dek = Zeroizing::new(
            decrypt_aead(
                Cipher::aes_256_gcm(),
                &self.key,
                Some(&envelope.wrapped_dek[..12]),
                &aad,
                &envelope.wrapped_dek[28..],
                &envelope.wrapped_dek[12..28],
            )
            .map_err(|_| CryptoError)?,
        );
        if dek.len() != 32 {
            return Err(CryptoError);
        }
        decrypt_aead(
            Cipher::aes_256_gcm(),
            &dek,
            Some(&envelope.nonce),
            &aad,
            &envelope.ciphertext,
            &envelope.tag,
        )
        .map(SensitiveValue::new)
        .map_err(|_| CryptoError)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn envelope_tamper_and_identity_swaps_fail_closed() {
        let crypto = EnvelopeCrypto {
            key: Arc::new(Zeroizing::new(vec![7; 32])),
        };
        let id = Identity {
            organization: Uuid::new_v4(),
            secret: Uuid::new_v4(),
            version: Uuid::new_v4(),
        };
        let value = SensitiveValue::new(b"not-in-a-database-dump\n".to_vec());
        let envelope = crypto.seal(id, &value).await.unwrap();
        assert_eq!(
            crypto.open(id, &envelope).await.unwrap().bytes(),
            value.bytes()
        );
        for wrong in [
            Identity {
                organization: Uuid::new_v4(),
                ..id
            },
            Identity {
                secret: Uuid::new_v4(),
                ..id
            },
            Identity {
                version: Uuid::new_v4(),
                ..id
            },
        ] {
            assert!(crypto.open(wrong, &envelope).await.is_err())
        }
        let mut tampered = envelope.clone();
        tampered.ciphertext[0] ^= 1;
        assert!(crypto.open(id, &tampered).await.is_err());
        assert!(!format!("{value:?}").contains("not-in"));
        assert!(!envelope
            .ciphertext
            .windows(value.bytes().len())
            .any(|s| s == value.bytes()));
    }
}

#[cfg(test)]
mod provider_tests {
    use super::*;
    #[tokio::test]
    async fn different_wrapping_key_and_randomized_versions() {
        let crypto = EnvelopeCrypto {
            key: Arc::new(Zeroizing::new(vec![1; 32])),
        };
        let wrong = EnvelopeCrypto {
            key: Arc::new(Zeroizing::new(vec![2; 32])),
        };
        let id = Identity {
            organization: Uuid::new_v4(),
            secret: Uuid::new_v4(),
            version: Uuid::new_v4(),
        };
        let value = SensitiveValue::new(b"byte-exact\n\0value".to_vec());
        let first = crypto.seal(id, &value).await.unwrap();
        let second = crypto.seal(id, &value).await.unwrap();
        assert_ne!(first.nonce, second.nonce);
        assert_ne!(first.wrapped_dek, second.wrapped_dek);
        assert_ne!(first.ciphertext, second.ciphertext);
        assert!(wrong.open(id, &first).await.is_err());
        assert_eq!(
            crypto.open(id, &first).await.unwrap().bytes(),
            value.bytes()
        );
    }
    #[test]
    #[cfg(unix)]
    fn key_requires_protected_regular_file() {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("key");
        std::fs::write(&path, [7; 32]).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(EnvelopeCrypto::key_file(&path).is_err());
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(EnvelopeCrypto::key_file(&path).is_ok());
        let symlink = temp.path().join("link");
        std::os::unix::fs::symlink(&path, &symlink).unwrap();
        assert!(EnvelopeCrypto::key_file(&symlink).is_err());
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o400)).unwrap();
        assert!(EnvelopeCrypto::key_file(&path).is_ok());
        assert!(EnvelopeCrypto::key_file(temp.path()).is_err());
        assert!(EnvelopeCrypto::key_file(&temp.path().join("missing")).is_err());
        let fifo = temp.path().join("fifo");
        let fifo_c = std::ffi::CString::new(fifo.as_os_str().as_encoded_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(fifo_c.as_ptr(), 0o600) }, 0);
        assert!(EnvelopeCrypto::key_file(&fifo).is_err());
        for length in [0, 31, 33, 4096] {
            let invalid = temp.path().join(format!("invalid-{length}"));
            std::fs::write(&invalid, vec![7; length]).unwrap();
            std::fs::set_permissions(&invalid, std::fs::Permissions::from_mode(0o600)).unwrap();
            assert!(EnvelopeCrypto::key_file(&invalid).is_err());
        }
    }
    #[tokio::test]
    #[cfg(unix)]
    async fn injected_read_only_key_survives_restart_without_cloud_credentials() {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("injected-key");
        let mut key = Zeroizing::new(vec![0; 32]);
        rand_bytes(&mut key).unwrap();
        std::fs::write(&path, &*key).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o400)).unwrap();
        let id = Identity {
            organization: Uuid::new_v4(),
            secret: Uuid::new_v4(),
            version: Uuid::new_v4(),
        };
        let value = SensitiveValue::new(b"persisted-through-restart".to_vec());
        let envelope = EnvelopeCrypto::key_file(&path)
            .unwrap()
            .seal(id, &value)
            .await
            .unwrap();
        let restarted = EnvelopeCrypto::key_file(&path).unwrap();
        assert_eq!(
            restarted.open(id, &envelope).await.unwrap().bytes(),
            value.bytes()
        );
        assert!(!envelope
            .wrapped_dek
            .windows(32)
            .any(|bytes| bytes == &**key));
        let mut tampered = envelope.clone();
        tampered.wrapped_dek[28] ^= 1;
        assert!(restarted.open(id, &tampered).await.is_err());
        tampered = envelope.clone();
        tampered.wrapping_key_id = "unknown-provider".into();
        assert!(restarted.open(id, &tampered).await.is_err());
    }
}
