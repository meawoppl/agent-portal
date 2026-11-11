//! Remove archived plaintext history only after a sanitized current object exists.
//! SDK errors may contain response payloads: this boundary returns fixed diagnostics.
use archive_format::ArchiveBackendConfig;

const PAGE_SIZE: i32 = 1000;
const MAX_PAGES: usize = 100_000;

#[derive(Debug, Clone, PartialEq, Eq)]
struct Head {
    version: String,
    etag: Option<String>,
}
#[derive(Debug, Clone)]
struct Version {
    key: String,
    version: String,
    latest: bool,
    marker: bool,
}
#[derive(Debug, Clone, PartialEq, Eq)]
struct Cursor {
    key: String,
    version: Option<String>,
}
#[derive(Default)]
struct Page {
    entries: Vec<Version>,
    next: Option<Cursor>,
}

trait VersionStore {
    async fn head(&self, key: &str) -> Result<Option<Head>, String>;
    async fn page(&self, key: &str, cursor: Option<&Cursor>) -> Result<Page, String>;
    async fn delete(&self, entries: &[Version]) -> Result<(), String>;
}
struct S3 {
    client: aws_sdk_s3::Client,
    bucket: String,
}
impl VersionStore for S3 {
    async fn head(&self, key: &str) -> Result<Option<Head>, String> {
        match self
            .client
            .head_object()
            .bucket(&self.bucket)
            .key(key)
            .send()
            .await
        {
            Ok(result) => Ok(Some(Head {
                version: result.version_id().unwrap_or("null").into(),
                etag: result.e_tag().map(str::to_owned),
            })),
            Err(error) if error.as_service_error().is_some_and(|e| e.is_not_found()) => Ok(None),
            Err(_) => Err("Archive version verification denied or unavailable".into()),
        }
    }
    async fn page(&self, key: &str, cursor: Option<&Cursor>) -> Result<Page, String> {
        let result = self
            .client
            .list_object_versions()
            .bucket(&self.bucket)
            .prefix(key)
            .max_keys(PAGE_SIZE)
            .set_key_marker(cursor.map(|c| c.key.clone()))
            .set_version_id_marker(cursor.and_then(|c| c.version.clone()))
            .send()
            .await
            .map_err(|_| "Archive version enumeration denied or unavailable".to_owned())?;
        let mut entries =
            Vec::with_capacity(result.versions().len() + result.delete_markers().len());
        for version in result.versions() {
            if version.key() != Some(key) {
                continue;
            }
            entries.push(Version {
                key: key.into(),
                version: version
                    .version_id()
                    .ok_or("Archive version identifier missing")?
                    .into(),
                latest: version.is_latest().unwrap_or(false),
                marker: false,
            });
        }
        for marker in result.delete_markers() {
            if marker.key() != Some(key) {
                continue;
            }
            entries.push(Version {
                key: key.into(),
                version: marker
                    .version_id()
                    .ok_or("Archive delete-marker identifier missing")?
                    .into(),
                latest: marker.is_latest().unwrap_or(false),
                marker: true,
            });
        }
        let next = if result.is_truncated().unwrap_or(false) {
            let next = Cursor {
                key: result
                    .next_key_marker()
                    .ok_or("Archive pagination marker missing")?
                    .into(),
                version: result.next_version_id_marker().map(str::to_owned),
            };
            // Prefix listings include keys following our exact target. No target
            // versions can occur after the service advances to a later key.
            if next.key.as_str() > key {
                None
            } else {
                Some(next)
            }
        } else {
            None
        };
        Ok(Page { entries, next })
    }
    async fn delete(&self, entries: &[Version]) -> Result<(), String> {
        for batch in entries.chunks(PAGE_SIZE as usize) {
            if batch.is_empty() {
                continue;
            }
            let objects = batch
                .iter()
                .map(|entry| {
                    aws_sdk_s3::types::ObjectIdentifier::builder()
                        .key(&entry.key)
                        .version_id(&entry.version)
                        .build()
                })
                .collect::<Result<Vec<_>, _>>()
                .map_err(|_| "Archive deletion identifiers invalid".to_owned())?;
            let delete = aws_sdk_s3::types::Delete::builder()
                .set_objects(Some(objects))
                .quiet(true)
                .build()
                .map_err(|_| "Archive deletion request invalid".to_owned())?;
            let result = self
                .client
                .delete_objects()
                .bucket(&self.bucket)
                .delete(delete)
                .send()
                .await
                .map_err(|_| {
                    "Archive version deletion denied, locked, or unavailable".to_owned()
                })?;
            // HTTP 200 can contain AccessDenied/ObjectLock failures per object.
            if !result.errors().is_empty() {
                return Err("Archive version deletion blocked for one or more objects".into());
            }
        }
        Ok(())
    }
}

async fn configured(config: &ArchiveBackendConfig, key: &str) -> Result<(S3, String), String> {
    let ArchiveBackendConfig::S3 { bucket, prefix } = config else {
        return Err("S3 version cleanup requires an S3 archive".into());
    };
    if key.is_empty() || key.starts_with('/') || key.split('/').any(|part| part == "..") {
        return Err("Invalid archive object key".into());
    }
    let sdk = aws_config::defaults(aws_config::BehaviorVersion::latest())
        .region(
            aws_config::meta::region::RegionProviderChain::default_provider().or_else("us-east-1"),
        )
        .load()
        .await;
    let mut builder = aws_sdk_s3::config::Builder::from(&sdk).timeout_config(
        aws_config::timeout::TimeoutConfig::builder()
            .operation_timeout(std::time::Duration::from_secs(60))
            .operation_attempt_timeout(std::time::Duration::from_secs(30))
            .build(),
    );
    let endpoint = std::env::var("AWS_ENDPOINT_URL_S3")
        .ok()
        .or_else(|| std::env::var("AWS_ENDPOINT_URL").ok())
        .or_else(|| std::env::var("AWS_ENDPOINT").ok());
    if let Some(endpoint) = endpoint {
        if endpoint.starts_with("http://")
            && !std::env::var("AWS_ALLOW_HTTP").is_ok_and(|v| v.eq_ignore_ascii_case("true"))
        {
            return Err("Archive HTTP endpoint requires AWS_ALLOW_HTTP=true".into());
        }
        builder = builder.endpoint_url(endpoint);
    }
    let virtual_hosted = match std::env::var("AWS_VIRTUAL_HOSTED_STYLE_REQUEST") {
        Ok(value) if value.eq_ignore_ascii_case("true") => true,
        Ok(value) if value.eq_ignore_ascii_case("false") => false,
        Err(_) => false,
        _ => return Err("Invalid archive virtual-hosted-style setting".into()),
    };
    builder = builder.force_path_style(!virtual_hosted);
    let key = match prefix {
        Some(prefix) => format!("{prefix}/{key}"),
        None => key.into(),
    };
    Ok((
        S3 {
            client: aws_sdk_s3::Client::from_conf(builder.build()),
            bucket: bucket.clone(),
        },
        key,
    ))
}

/// Keep the sanitized current object, removing every older version and marker.
/// Concurrent replacement is reported as blocked rather than declaring success.
pub async fn purge_superseded(config: &ArchiveBackendConfig, key: &str) -> Result<(), String> {
    let (store, key) = configured(config, key).await?;
    cleanup(&store, &key, false).await
}
/// Remove all retained versions of an expendable media object, including null versions.
pub async fn delete_all_versions(config: &ArchiveBackendConfig, key: &str) -> Result<(), String> {
    let (store, key) = configured(config, key).await?;
    cleanup(&store, &key, true).await
}
async fn cleanup(store: &impl VersionStore, key: &str, all: bool) -> Result<(), String> {
    let current = store.head(key).await?;
    if !all
        && current
            .as_ref()
            .is_some_and(|head| head.version == "null" && head.etag.is_none())
    {
        return Err("Unversioned archive object lacks a verification ETag".into());
    }
    if !all && current.is_none() {
        return Err("Sanitized current archive object is missing".into());
    }
    let mut cursor = None;
    let mut complete = false;
    let mut saw_null = false;
    for _ in 0..MAX_PAGES {
        let page = store.page(key, cursor.as_ref()).await?;
        let mut targets = Vec::with_capacity(page.entries.len());
        for entry in page.entries {
            if entry.key != key {
                continue;
            }
            saw_null |= entry.version == "null";
            if !all && entry.latest && entry.marker {
                return Err("Archive current object changed to a delete marker".into());
            }
            if all
                || (!entry.latest
                    && current
                        .as_ref()
                        .is_none_or(|head| head.version != entry.version))
            {
                targets.push(entry);
            }
        }
        store.delete(&targets).await?;
        if page.next.is_none() {
            complete = true;
            break;
        }
        if page.next == cursor {
            return Err("Archive pagination did not advance".into());
        }
        cursor = page.next;
    }
    if !complete {
        return Err("Archive version enumeration exceeded safety limit".into());
    }
    // Some S3-compatible unversioned stores omit the live null object from
    // ListObjectVersions. Explicit null deletion cannot create a delete marker.
    if all && !saw_null && current.as_ref().is_some_and(|head| head.version == "null") {
        store
            .delete(&[Version {
                key: key.into(),
                version: "null".into(),
                latest: true,
                marker: false,
            }])
            .await?;
    }
    let after = store.head(key).await?;
    if (all && after.is_some()) || (!all && after != current) {
        return Err("Archive current object changed during version cleanup".into());
    }
    // Independent bounded verification catches partial S3-compatible deletes,
    // concurrent writes, and per-object failures that a service failed to report.
    let mut cursor = None;
    for _ in 0..MAX_PAGES {
        let page = store.page(key, cursor.as_ref()).await?;
        if page.entries.iter().any(|entry| {
            entry.key == key
                && (all
                    || entry.marker
                    || !entry.latest
                    || current
                        .as_ref()
                        .is_none_or(|head| head.version != entry.version))
        }) {
            return Err("Archive historical versions remain after cleanup".into());
        }
        if page.next.is_none() {
            return Ok(());
        }
        if page.next == cursor {
            return Err("Archive verification pagination did not advance".into());
        }
        cursor = page.next;
    }
    Err("Archive version verification exceeded safety limit".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{collections::VecDeque, sync::Mutex};
    fn read_request(socket: &mut std::net::TcpStream) -> String {
        use std::io::Read;
        let mut bytes = Vec::new();
        loop {
            let mut chunk = [0u8; 4096];
            let count = socket.read(&mut chunk).unwrap();
            assert!(count > 0 && bytes.len() + count <= 65536);
            bytes.extend_from_slice(&chunk[..count]);
            if let Some(end) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
                let headers = String::from_utf8_lossy(&bytes[..end]).to_ascii_lowercase();
                let length: usize = headers
                    .lines()
                    .find_map(|line| line.strip_prefix("content-length:").map(str::trim))
                    .unwrap_or("0")
                    .parse()
                    .unwrap();
                if bytes.len() >= end + 4 + length {
                    break;
                }
            }
        }
        String::from_utf8(bytes).unwrap()
    }
    struct Mock {
        heads: Mutex<VecDeque<Option<Head>>>,
        pages: Mutex<VecDeque<(Option<Cursor>, Page)>>,
        deleted: Mutex<Vec<(String, String)>>,
        deny: bool,
    }
    impl VersionStore for Mock {
        async fn head(&self, _: &str) -> Result<Option<Head>, String> {
            Ok(self.heads.lock().unwrap().pop_front().unwrap())
        }
        async fn page(&self, _: &str, cursor: Option<&Cursor>) -> Result<Page, String> {
            let (expected, page) = self.pages.lock().unwrap().pop_front().unwrap();
            assert_eq!(cursor, expected.as_ref());
            Ok(page)
        }
        async fn delete(&self, entries: &[Version]) -> Result<(), String> {
            if self.deny {
                return Err("Archive version deletion blocked".into());
            }
            self.deleted.lock().unwrap().extend(
                entries
                    .iter()
                    .map(|entry| (entry.key.clone(), entry.version.clone())),
            );
            Ok(())
        }
    }
    fn version(id: &str, latest: bool, marker: bool) -> Version {
        Version {
            key: "target".into(),
            version: id.into(),
            latest,
            marker,
        }
    }
    fn head(id: &str) -> Option<Head> {
        Some(Head {
            version: id.into(),
            etag: Some("sanitized".into()),
        })
    }
    #[tokio::test]
    async fn paginates_versions_and_markers_preserves_current_and_verifies() {
        let cursor = Cursor {
            key: "target".into(),
            version: Some("older".into()),
        };
        let mock = Mock {
            heads: Mutex::new(VecDeque::from([head("current"), head("current")])),
            pages: Mutex::new(VecDeque::from([
                (
                    None,
                    Page {
                        entries: vec![
                            version("current", true, false),
                            version("older", false, false),
                        ],
                        next: Some(cursor.clone()),
                    },
                ),
                (
                    Some(cursor),
                    Page {
                        entries: vec![
                            version("marker", false, true),
                            Version {
                                key: "target-neighbor".into(),
                                ..version("neighbor", false, false)
                            },
                        ],
                        next: None,
                    },
                ),
                (
                    None,
                    Page {
                        entries: vec![version("current", true, false)],
                        next: None,
                    },
                ),
            ])),
            deleted: Mutex::new(Vec::new()),
            deny: false,
        };
        cleanup(&mock, "target", false).await.unwrap();
        assert_eq!(
            *mock.deleted.lock().unwrap(),
            [
                ("target".into(), "older".into()),
                ("target".into(), "marker".into())
            ]
        );
        assert!(mock.pages.lock().unwrap().is_empty());
    }
    #[tokio::test]
    async fn null_version_is_preserved_for_sanitized_object_and_deleted_for_media() {
        for all in [false, true] {
            let current = version("null", true, false);
            let mock = Mock {
                heads: Mutex::new(VecDeque::from([
                    head("null"),
                    if all { None } else { head("null") },
                ])),
                pages: Mutex::new(VecDeque::from([
                    (
                        None,
                        Page {
                            entries: vec![current.clone()],
                            next: None,
                        },
                    ),
                    (
                        None,
                        Page {
                            entries: if all { vec![] } else { vec![current] },
                            next: None,
                        },
                    ),
                ])),
                deleted: Mutex::new(Vec::new()),
                deny: false,
            };
            cleanup(&mock, "target", all).await.unwrap();
            assert_eq!(mock.deleted.lock().unwrap().len(), usize::from(all));
            if all {
                assert_eq!(mock.deleted.lock().unwrap()[0].1, "null");
            }
        }
    }
    #[tokio::test]
    async fn unversioned_compatible_store_can_omit_live_null_from_listing() {
        let mock = Mock {
            heads: Mutex::new(VecDeque::from([head("null"), None])),
            pages: Mutex::new(VecDeque::from([
                (None, Page::default()),
                (None, Page::default()),
            ])),
            deleted: Mutex::new(Vec::new()),
            deny: false,
        };
        cleanup(&mock, "target", true).await.unwrap();
        assert_eq!(
            *mock.deleted.lock().unwrap(),
            [("target".into(), "null".into())]
        );
    }
    #[tokio::test]
    async fn permissions_partial_delete_and_concurrent_replacement_block_completion() {
        for mode in 0..3 {
            let mock = Mock {
                heads: Mutex::new(VecDeque::from([
                    head("current"),
                    if mode == 2 {
                        head("replacement")
                    } else {
                        head("current")
                    },
                ])),
                pages: Mutex::new(VecDeque::from([
                    (
                        None,
                        Page {
                            entries: vec![version("old", false, false)],
                            next: None,
                        },
                    ),
                    (
                        None,
                        Page {
                            entries: vec![
                                version("current", true, false),
                                version("old", false, false),
                            ],
                            next: None,
                        },
                    ),
                ])),
                deleted: Mutex::new(Vec::new()),
                deny: mode == 0,
            };
            assert!(cleanup(&mock, "target", false).await.is_err());
        }
    }
    #[tokio::test]
    async fn repeated_pagination_cursor_is_blocked() {
        let cursor = Cursor {
            key: "target".into(),
            version: Some("old".into()),
        };
        let mock = Mock {
            heads: Mutex::new(VecDeque::from([head("current")])),
            pages: Mutex::new(VecDeque::from([
                (
                    None,
                    Page {
                        entries: vec![],
                        next: Some(cursor.clone()),
                    },
                ),
                (
                    Some(cursor.clone()),
                    Page {
                        entries: vec![],
                        next: Some(cursor),
                    },
                ),
            ])),
            deleted: Mutex::new(Vec::new()),
            deny: false,
        };
        assert!(cleanup(&mock, "target", false)
            .await
            .unwrap_err()
            .contains("pagination"));
    }
    #[tokio::test]
    async fn s3_requests_carry_both_pagination_markers_and_parse_version_flags() {
        use std::io::Write;
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let pages = [
                "<ListVersionsResult xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\"><IsTruncated>true</IsTruncated><NextKeyMarker>target</NextKeyMarker><NextVersionIdMarker>older</NextVersionIdMarker><Version><Key>target</Key><VersionId>current</VersionId><IsLatest>true</IsLatest></Version><Version><Key>target</Key><VersionId>older</VersionId><IsLatest>false</IsLatest></Version></ListVersionsResult>",
                "<ListVersionsResult xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\"><IsTruncated>false</IsTruncated><DeleteMarker><Key>target</Key><VersionId>marker</VersionId><IsLatest>false</IsLatest></DeleteMarker></ListVersionsResult>",
            ];
            let mut requests = Vec::new();
            for body in pages {
                let (mut socket, _) = listener.accept().unwrap();
                socket
                    .set_read_timeout(Some(std::time::Duration::from_secs(10)))
                    .unwrap();
                requests.push(read_request(&mut socket));
                write!(socket, "HTTP/1.1 200 OK\r\nContent-Type: application/xml\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body).unwrap();
            }
            requests
        });
        let config = aws_sdk_s3::config::Builder::new()
            .behavior_version(aws_config::BehaviorVersion::latest())
            .region(aws_sdk_s3::config::Region::new("us-east-1"))
            .credentials_provider(aws_sdk_s3::config::Credentials::new(
                "test",
                "test",
                None,
                None,
                "local-test",
            ))
            .endpoint_url(format!("http://{address}"))
            .force_path_style(true)
            .build();
        let store = S3 {
            client: aws_sdk_s3::Client::from_conf(config),
            bucket: "test".into(),
        };
        let first = store.page("target", None).await.unwrap();
        assert_eq!(first.entries.len(), 2);
        assert!(first.entries[0].latest);
        assert!(!first.entries[1].latest);
        let second = store.page("target", first.next.as_ref()).await.unwrap();
        assert!(second.next.is_none());
        assert!(second.entries[0].marker);
        let requests = server.join().unwrap();
        assert!(requests[0].contains("versions"));
        assert!(requests[0].contains("max-keys=1000"));
        assert!(requests[1].contains("key-marker=target"));
        assert!(requests[1].contains("version-id-marker=older"));
    }
    #[tokio::test]
    async fn s3_http_200_object_lock_failure_is_not_success() {
        use std::io::Write;
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(std::time::Duration::from_secs(10)))
                .unwrap();
            let request = read_request(&mut socket);
            let body = "<DeleteResult xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\"><Error><Key>target</Key><VersionId>old</VersionId><Code>AccessDenied</Code><Message>SENSITIVE-ECHO</Message></Error></DeleteResult>";
            write!(socket, "HTTP/1.1 200 OK\r\nContent-Type: application/xml\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body).unwrap();
            request
        });
        let config = aws_sdk_s3::config::Builder::new()
            .behavior_version(aws_config::BehaviorVersion::latest())
            .region(aws_sdk_s3::config::Region::new("us-east-1"))
            .credentials_provider(aws_sdk_s3::config::Credentials::new(
                "test",
                "test",
                None,
                None,
                "local-test",
            ))
            .endpoint_url(format!("http://{address}"))
            .force_path_style(true)
            .build();
        let store = S3 {
            client: aws_sdk_s3::Client::from_conf(config),
            bucket: "test".into(),
        };
        let error = store
            .delete(&[version("old", false, false)])
            .await
            .unwrap_err();
        assert!(!error.contains("SENSITIVE-ECHO"));
        assert!(error.contains("blocked"));
        let request = server.join().unwrap();
        assert!(
            request.starts_with("POST /test/?delete") || request.starts_with("POST /test?delete")
        );
    }
}
