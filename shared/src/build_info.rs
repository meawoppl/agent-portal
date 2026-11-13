//! Build identity belongs to an executable, not the reusable protocol crate.
//!
//! Native entry points register their own identity before starting sessions.
//! Library-only consumers without an executable identity report `unknown`,
//! never a stale commit baked into a cached dependency.

use std::sync::OnceLock;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BuildInfo {
    pub version: &'static str,
    pub git_hash: &'static str,
    pub build_time: &'static str,
}

impl BuildInfo {
    pub const UNKNOWN: Self = Self {
        version: "unknown",
        git_hash: "unknown",
        build_time: "unknown",
    };
}

static RUNNING: OnceLock<BuildInfo> = OnceLock::new();

/// Initialize once at the binary boundary. Conflicting identities are refused;
/// repeat initialization with the same identity is harmless.
pub fn register(info: BuildInfo) -> Result<(), BuildInfo> {
    let actual = *RUNNING.get_or_init(|| info);
    if actual == info {
        Ok(())
    } else {
        Err(actual)
    }
}

pub fn running() -> BuildInfo {
    RUNNING.get().copied().unwrap_or(BuildInfo::UNKNOWN)
}

/// Expand in a crate whose build script calls `build-support/identity.rs`.
/// `env!` resolves in that crate, keeping git changes out of shared fingerprints.
#[macro_export]
macro_rules! build_info {
    () => {
        $crate::build_info::BuildInfo {
            version: env!("PORTAL_VERSION"),
            git_hash: env!("PORTAL_GIT_HASH"),
            build_time: env!("PORTAL_BUILD_TIME"),
        }
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registration_is_explicit_idempotent_and_rejects_conflicts() {
        assert_eq!(running(), BuildInfo::UNKNOWN);
        let info = BuildInfo {
            version: "2.15.1234",
            git_hash: "a1b2c3d4",
            build_time: "fixture",
        };
        assert_eq!(register(info), Ok(()));
        assert_eq!(running(), info);
        assert_eq!(register(info), Ok(()));
        assert_eq!(register(BuildInfo::UNKNOWN), Err(info));
        assert_eq!(running(), info);
    }
}
