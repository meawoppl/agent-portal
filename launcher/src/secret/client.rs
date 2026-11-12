use super::sensitive::SecretString;
use anyhow::{bail, Context};
use reqwest::{Method, Url};
use serde::{de::DeserializeOwned, Serialize};
use zeroize::Zeroizing;

#[derive(Debug)]
pub struct ServiceError {
    pub status: u16,
    message: &'static str,
}
impl std::fmt::Display for ServiceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.message)
    }
}
impl std::error::Error for ServiceError {}

pub struct Client {
    pub base: String,
    http: reqwest::Client,
    credential: Option<SecretString>,
}
impl Client {
    pub fn new(base: &str, dev: bool, credential: Option<SecretString>) -> anyhow::Result<Self> {
        let base = shared::urls::ws_to_http(base);
        let url = Url::parse(&base).context("Invalid backend URL")?;
        let local = matches!(
            url.host_str(),
            Some("localhost" | "127.0.0.1" | "[::1]" | "::1")
        );
        if url.scheme() != "https" && !(dev && local && url.scheme() == "http") {
            bail!("Secret operations require HTTPS (explicit --dev permits loopback HTTP)");
        }
        if !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            bail!("Backend URL cannot contain credentials, query parameters, or a fragment");
        }
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(std::time::Duration::from_secs(30))
            .build()?;
        Ok(Self {
            base: base.trim_end_matches('/').into(),
            http,
            credential,
        })
    }
    pub async fn request<T: DeserializeOwned, B: Serialize + ?Sized>(
        &self,
        method: Method,
        path: &str,
        body: Option<&B>,
    ) -> anyhow::Result<T> {
        self.request_with_token(
            method,
            path,
            body,
            self.credential.as_ref().map(SecretString::as_str),
        )
        .await
    }
    pub async fn request_with_token<T: DeserializeOwned, B: Serialize + ?Sized>(
        &self,
        method: Method,
        path: &str,
        body: Option<&B>,
        token: Option<&str>,
    ) -> anyhow::Result<T> {
        let mutation = method != Method::GET;
        let mut request = self
            .http
            .request(method, format!("{}/api/secrets{}", self.base, path));
        if let Some(token) = token {
            request = request.bearer_auth(token);
        }
        if mutation {
            request = request.header("Idempotency-Key", uuid::Uuid::new_v4().to_string());
        }
        // Serialize into a zeroizing buffer; never attach sensitive request Debug to an error.
        let encoded = body
            .map(serde_json::to_vec)
            .transpose()
            .map_err(|_| anyhow::anyhow!("Could not encode secret request"))?
            .map(Zeroizing::new);
        if let Some(encoded) = &encoded {
            request = request
                .header("Content-Type", "application/json")
                .body(encoded.to_vec());
        }
        let mut response = request.send().await.map_err(|_| {
            anyhow::anyhow!("Secret service request failed; no response body is logged")
        })?;
        let status = response.status();
        if !status.is_success() {
            return Err(ServiceError {
                status: status.as_u16(),
                message: match status.as_u16() {
                    401 => "Bound session credential rejected or expired; reconnect this portal session",
                    403 => "Secret operation denied by policy",
                    404 => "Secret, version, or verified member not found",
                    409 => "Secret changed concurrently; inspect metadata and try again",
                    413 => "Secret input exceeds the service limit",
                    _ => "Secret service rejected the request; consult service diagnostics",
                },
            }
            .into());
        }
        let mut bytes = Zeroizing::new(Vec::new());
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| anyhow::anyhow!("Could not receive secret response"))?
        {
            if bytes.len() + chunk.len() > 4 * 1024 * 1024 {
                bail!("Secret service response exceeds safety limit");
            }
            bytes.extend_from_slice(&chunk);
        }
        serde_json::from_slice(&bytes)
            .map_err(|_| anyhow::anyhow!("Invalid secret service response"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
    /// A hostile server may echo a value in either an error body or malformed JSON.
    /// Neither is allowed to become a diagnostic displayed in the transcript.
    async fn mock_response(status: &str, body: &str) -> (String, std::thread::JoinHandle<String>) {
        use std::io::Write;
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let response = format!(
            "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let handle = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                .unwrap();
            let request = read_request(&mut socket);
            socket.write_all(response.as_bytes()).unwrap();
            request
        });
        (format!("http://{address}"), handle)
    }
    #[tokio::test]
    async fn errors_never_echo_sensitive_response_and_bound_bearer_is_used() {
        let (base, server) = mock_response("403 Forbidden", "SUPER-SECRET-ECHO").await;
        let client = Client::new(
            &base,
            true,
            Some(SecretString::new("SESSION-PROXY-TOKEN".into())),
        )
        .unwrap();
        let result = client
            .request::<shared::api::SecretBindingTicketResponse, ()>(
                Method::POST,
                "/bindings/00000000-0000-0000-0000-000000000001/ticket",
                None,
            )
            .await
            .unwrap_err();
        assert!(!format!("{result:?}").contains("SUPER-SECRET"));
        let request = server.join().unwrap().to_ascii_lowercase();
        assert!(request.contains("authorization: bearer session-proxy-token"));
        assert!(request.contains("idempotency-key:"));
    }
    #[tokio::test]
    async fn malformed_value_response_is_not_in_parse_error() {
        let (base, server) = mock_response("200 OK", "SECRET-VALUE-BAD-JSON").await;
        let client = Client::new(&base, true, None).unwrap();
        let error = client
            .request::<shared::api::SecretBindingRedeemResponse, ()>(Method::GET, "/test", None)
            .await
            .unwrap_err();
        assert!(!format!("{error:?}").contains("SECRET-VALUE"));
        server.join().unwrap();
    }
}
