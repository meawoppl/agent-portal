//! Read a pinned data-only readiness commit, then immutable source artifacts.
use anyhow::{bail, Context, Result};
use serde::Deserialize;
use std::collections::BTreeMap;

use crate::{GitHubRelease, Platform, GITHUB_REPO};

#[derive(Deserialize)]
struct GitReference {
    object: GitObject,
}
#[derive(Deserialize)]
struct GitObject {
    sha: String,
    #[serde(rename = "type")]
    kind: String,
}
#[derive(Debug, Deserialize)]
struct ReadinessPointer {
    schema: u32,
    channel: String,
    source: String,
}

impl ReadinessPointer {
    fn source(self, target: &str) -> Result<String> {
        if self.schema != 1 || self.channel != target || !hex_sha(&self.source, 40) {
            bail!("Invalid ready-platform source pointer");
        }
        Ok(self.source)
    }
}

fn pointer_url(commit: &str) -> String {
    format!("https://raw.githubusercontent.com/{GITHUB_REPO}/{commit}/readiness.json")
}

#[derive(Debug, Deserialize)]
struct ReadyManifest {
    schema: u32,
    commit: String,
    version: String,
    platform: String,
    artifacts: BTreeMap<String, ReadyArtifact>,
}
#[derive(Debug, Deserialize)]
struct ReadyArtifact {
    sha256: String,
    size: u64,
}

pub(crate) struct Available {
    pub version: String,
    pub download_url: String,
    pub digest: Option<String>,
    pub numeric_version: Option<(u64, u64, u64)>,
}

pub(crate) fn numeric_version(value: &str) -> Option<(u64, u64, u64)> {
    let mut parts = value.split('.');
    let parsed = (
        parts.next()?.parse().ok()?,
        parts.next()?.parse().ok()?,
        parts.next()?.parse().ok()?,
    );
    parts.next().is_none().then_some(parsed)
}

fn hex_sha(value: &str, len: usize) -> bool {
    value.len() == len
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn select_ready(
    manifest: ReadyManifest,
    release: &GitHubRelease,
    sha: &str,
    platform: &Platform,
) -> Result<Available> {
    let target = format!("{}-{}", platform.os, platform.arch);
    if manifest.schema != 1
        || manifest.commit != sha
        || manifest.platform != target
        || release.tag_name != format!("cli-{sha}")
        || release.draft
        || release.prerelease
    {
        bail!("Ready platform metadata does not match the source release");
    }
    let version = numeric_version(&manifest.version).context("Invalid ready platform version")?;
    let expected_names: Vec<_> = ["agent-portal", "claude-portal"]
        .iter()
        .map(|prefix| crate::binary_name_for(prefix, platform.os, platform.arch))
        .collect();
    if manifest.artifacts.len() != expected_names.len() {
        bail!("Ready manifest must describe exactly one launcher/proxy pair");
    }
    for name in expected_names {
        let entry = manifest
            .artifacts
            .get(&name)
            .context("Incomplete ready manifest")?;
        let asset = release
            .assets
            .iter()
            .find(|a| a.name == name)
            .context("Ready asset missing from release")?;
        if !hex_sha(&entry.sha256, 64)
            || entry.size == 0
            || asset.size != entry.size
            || asset.digest.as_deref() != Some(format!("sha256:{}", entry.sha256).as_str())
        {
            bail!("Ready asset checksum or size mismatch");
        }
    }
    let entry = manifest
        .artifacts
        .get(&platform.binary_name)
        .context("Unsupported binary prefix")?;
    Ok(Available {
        version: manifest.version,
        download_url: format!(
            "https://github.com/{GITHUB_REPO}/releases/download/cli-{sha}/{}",
            platform.binary_name
        ),
        digest: Some(format!("sha256:{}", entry.sha256)),
        numeric_version: Some(version),
    })
}

pub(crate) async fn available(client: &reqwest::Client, platform: &Platform) -> Result<Available> {
    let target = format!("{}-{}", platform.os, platform.arch);
    let reference = client
        .get(format!(
            "https://api.github.com/repos/{GITHUB_REPO}/git/ref/heads/release-ready-v2/{target}"
        ))
        .send()
        .await
        .context("Could not check platform readiness")?;
    // Only absence permits the migration fallback. An outage or invalid
    // readiness record must never silently choose a different release.
    if reference.status() == reqwest::StatusCode::NOT_FOUND {
        let release: GitHubRelease = client
            .get(format!(
                "https://api.github.com/repos/{GITHUB_REPO}/releases/tags/latest"
            ))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        let asset = release
            .assets
            .iter()
            .find(|a| a.name == platform.binary_name)
            .context("Platform is unavailable in the complete legacy release; retry later")?;
        return Ok(Available {
            version: release.name,
            download_url: asset.browser_download_url.clone(),
            digest: asset.digest.clone(),
            numeric_version: None,
        });
    }
    let reference: GitReference = reference.error_for_status()?.json().await?;
    if reference.object.kind != "commit" || !hex_sha(&reference.object.sha, 40) {
        bail!("Invalid ready-platform source reference");
    }
    // Pin the data read to the ref's commit, not its mutable branch name.
    // A concurrent publisher can advance the ref without mixing snapshots.
    let pointer: ReadinessPointer = client
        .get(pointer_url(&reference.object.sha))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let sha = pointer.source(&target)?;
    let release: GitHubRelease = client
        .get(format!(
            "https://api.github.com/repos/{GITHUB_REPO}/releases/tags/cli-{sha}"
        ))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let manifest: ReadyManifest = client
        .get(format!(
            "https://github.com/{GITHUB_REPO}/releases/download/cli-{sha}/ready-{target}.json"
        ))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    select_ready(manifest, &release, &sha, platform)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::GitHubAsset;

    fn fixture() -> (ReadyManifest, GitHubRelease, Platform) {
        let platform = Platform {
            os: "linux",
            arch: "x86_64",
            binary_name: "agent-portal-linux-x86_64".into(),
        };
        let artifacts: BTreeMap<String, ReadyArtifact> =
            ["agent-portal-linux-x86_64", "claude-portal-linux-x86_64"]
                .into_iter()
                .map(|name| {
                    (
                        name.into(),
                        ReadyArtifact {
                            sha256: "a".repeat(64),
                            size: 10,
                        },
                    )
                })
                .collect();
        let assets = artifacts
            .keys()
            .map(|name| GitHubAsset {
                name: name.clone(),
                browser_download_url: "ignored mutable URL".into(),
                digest: Some(format!("sha256:{}", "a".repeat(64))),
                size: 10,
            })
            .collect();
        (
            ReadyManifest {
                schema: 1,
                commit: "b".repeat(40),
                version: "2.15.1910".into(),
                platform: "linux-x86_64".into(),
                artifacts,
            },
            GitHubRelease {
                tag_name: format!("cli-{}", "b".repeat(40)),
                name: "ready".into(),
                assets,
                draft: false,
                prerelease: false,
            },
            platform,
        )
    }

    #[test]
    fn pointer_schema_channel_and_source_are_validated_and_fetch_is_pinned() {
        let data_commit = "d".repeat(40);
        assert_eq!(
            pointer_url(&data_commit),
            format!("https://raw.githubusercontent.com/{GITHUB_REPO}/{data_commit}/readiness.json")
        );
        for (schema, channel, source, valid) in [
            (1, "linux-x86_64", "a".repeat(40), true),
            (2, "linux-x86_64", "a".repeat(40), false),
            (1, "windows-x86_64", "a".repeat(40), false),
            (1, "linux-x86_64", "../main".into(), false),
        ] {
            let pointer = ReadinessPointer {
                schema,
                channel: channel.into(),
                source,
            };
            assert_eq!(pointer.source("linux-x86_64").is_ok(), valid);
        }
    }

    #[test]
    fn ready_linux_does_not_require_delayed_windows_assets() {
        let (manifest, release, platform) = fixture();
        let selected = select_ready(manifest, &release, &"b".repeat(40), &platform).unwrap();
        assert!(selected
            .download_url
            .contains(&format!("/cli-{}/", "b".repeat(40))));
        assert_eq!(selected.numeric_version, Some((2, 15, 1910)));
        assert!(
            numeric_version("2.15.1911") > selected.numeric_version,
            "newer local build stays ahead"
        );
    }

    #[test]
    fn incomplete_or_mixed_commit_pairs_are_never_ready() {
        let (manifest, mut release, platform) = fixture();
        release.assets.pop();
        assert!(select_ready(manifest, &release, &"b".repeat(40), &platform).is_err());
        let (mut manifest, release, platform) = fixture();
        manifest.commit = "c".repeat(40);
        assert!(select_ready(manifest, &release, &"b".repeat(40), &platform).is_err());
        let (manifest, mut release, platform) = fixture();
        release.assets[0].digest = Some("sha256:wrong".into());
        assert!(select_ready(manifest, &release, &"b".repeat(40), &platform).is_err());
    }
}
