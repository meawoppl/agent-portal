use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

pub fn home_dir() -> Result<PathBuf> {
    dirs::home_dir().context("Could not determine launcher user's home directory")
}

pub fn expand_home_path(path: &str) -> Result<PathBuf> {
    if path == "~" || path == "~/" {
        return home_dir();
    }

    if let Some(rest) = path.strip_prefix("~/") {
        return Ok(home_dir()?.join(rest));
    }

    Ok(PathBuf::from(path))
}

pub fn ensure_existing_dir_under_home(path: &str) -> Result<PathBuf> {
    let requested = expand_home_path(path)?;
    let canonical = requested.canonicalize().with_context(|| {
        format!(
            "Working directory does not exist or is not readable: {}",
            requested.display()
        )
    })?;

    if !canonical.is_dir() {
        anyhow::bail!(
            "Working directory is not a directory: {}",
            canonical.display()
        );
    }

    ensure_canonical_path_under_home(&canonical)?;
    Ok(canonical)
}

pub fn ensure_canonical_path_under_home(path: &Path) -> Result<()> {
    let home = home_dir()?
        .canonicalize()
        .context("Could not resolve launcher user's home directory")?;

    if !path.starts_with(&home) {
        anyhow::bail!(
            "Launcher can only access paths under the user's home directory: {}",
            home.display()
        );
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tilde_expands_to_home() {
        assert_eq!(expand_home_path("~").unwrap(), home_dir().unwrap());
    }

    #[test]
    fn home_directory_is_allowed() {
        let home = home_dir().unwrap();
        let allowed = ensure_existing_dir_under_home(&home.to_string_lossy()).unwrap();
        assert!(allowed.starts_with(home.canonicalize().unwrap()));
    }

    #[test]
    fn temp_directory_is_rejected_when_outside_home() {
        let temp = std::env::temp_dir().canonicalize().unwrap();
        let home = home_dir().unwrap().canonicalize().unwrap();

        if !temp.starts_with(home) {
            let err = ensure_existing_dir_under_home(&temp.to_string_lossy()).unwrap_err();
            assert!(err.to_string().contains("home directory"));
        }
    }
}
