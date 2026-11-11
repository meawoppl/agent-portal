use super::sensitive::SecretString;
use anyhow::{bail, Context};
use serde::{Deserialize, Serialize};
use std::path::Path;
use zeroize::Zeroizing;

#[derive(Serialize, Deserialize)]
struct StoredCredential {
    backend: String,
    credential: shared::api::SecretValue,
    expires_at: Option<String>,
}
impl Drop for StoredCredential {
    fn drop(&mut self) {
        use zeroize::Zeroize;
        self.credential.0.zeroize();
    }
}

pub fn load(path: &Path, backend: &str) -> anyhow::Result<SecretString> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options
        .open(path)
        .context("Run `agent-portal secret authorize` to approve this CLI")?;
    if !file.metadata()?.is_file() {
        bail!("Secret credential must be a regular file");
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let metadata = file.metadata()?;
        if metadata.mode() & 0o077 != 0 || metadata.uid() != unsafe { libc::geteuid() } {
            bail!("Secret credential file must be owned by this user with mode 0600");
        }
    }
    let mut bytes = Zeroizing::new(Vec::new());
    use std::io::Read;
    file.take(16 * 1024).read_to_end(&mut bytes)?;
    let stored: StoredCredential = serde_json::from_slice(&bytes)
        .map_err(|_| anyhow::anyhow!("Invalid secret credential file; authorize again"))?;
    if stored.backend != backend {
        bail!(
            "Secret credential belongs to a different backend; authorize this backend explicitly"
        );
    }
    if !stored.credential.0.starts_with("aps_secret_") {
        bail!("Invalid dedicated secret credential; authorize again");
    }
    Ok(SecretString::new(stored.credential.0.clone()))
}

pub fn save(
    path: &Path,
    backend: &str,
    mut credential: String,
    expires_at: Option<String>,
) -> anyhow::Result<()> {
    if !credential.starts_with("aps_secret_") || credential.len() > 4096 {
        use zeroize::Zeroize;
        credential.zeroize();
        bail!("Invalid dedicated secret credential received");
    }
    let stored = StoredCredential {
        backend: backend.into(),
        credential: shared::api::SecretValue(credential),
        expires_at,
    };
    let bytes = Zeroizing::new(
        serde_json::to_vec(&stored)
            .map_err(|_| anyhow::anyhow!("Could not encode secret credential"))?,
    );
    let parent = path.parent().context("Invalid secret credential path")?;
    if !parent.exists() {
        let mut builder = std::fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder.create(parent)?;
    }
    let temporary = parent.join(format!(".secret-credential-{}", uuid::Uuid::new_v4()));
    let mut options = std::fs::OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let result = (|| -> anyhow::Result<()> {
        use std::io::Write;
        let mut file = options.open(&temporary)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        std::fs::rename(&temporary, path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result.context("Could not save dedicated secret credential")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn credential_is_backend_scoped_and_separate() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("secrets.json");
        save(
            &path,
            "https://example.test",
            "aps_secret_test".into(),
            None,
        )
        .unwrap();
        assert_eq!(
            &*load(&path, "https://example.test").unwrap(),
            "aps_secret_test"
        );
        assert!(load(&path, "https://other.test").is_err());
        #[cfg(unix)]
        {
            use std::os::unix::fs::{MetadataExt, PermissionsExt};
            assert_eq!(std::fs::metadata(&path).unwrap().mode() & 0o777, 0o600);
            let link = directory.path().join("linked-credential.json");
            std::os::unix::fs::symlink(&path, &link).unwrap();
            assert!(load(&link, "https://example.test").is_err());
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
            assert!(load(&path, "https://example.test").is_err());
        }
    }
}
