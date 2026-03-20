//! Download handlers for serving the portal binary and install script

use axum::{
    body::Body,
    extract::{Query, State},
    http::{header, Method, StatusCode},
    response::{IntoResponse, Response},
};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::sync::Arc;
use tokio_util::io::ReaderStream;

use crate::AppState;

#[derive(Deserialize)]
pub struct InstallScriptParams {
    /// Backend URL (WebSocket URL for runtime connection)
    backend_url: Option<String>,
}

/// Serve the install script that downloads and sets up the portal
pub async fn install_script(
    State(app_state): State<Arc<AppState>>,
    Query(params): Query<InstallScriptParams>,
) -> impl IntoResponse {
    // Use provided backend_url or derive from public_url
    let backend_url = params.backend_url.unwrap_or_else(|| {
        app_state
            .public_url
            .replace("https://", "wss://")
            .replace("http://", "ws://")
    });

    let script = format!(
        r##"#!/bin/bash
# Agent Portal Installer
# Downloads and installs the agent-portal binary and configures backend URL.

set -e

CONFIG_DIR="${{HOME}}/.config/claude-portal"
BIN_NAME="agent-portal"
BIN_PATH="${{CONFIG_DIR}}/${{BIN_NAME}}"
CONFIG_FILE="${{CONFIG_DIR}}/launcher.toml"
GITHUB_RELEASE_URL="https://github.com/meawoppl/agent-portal/releases/download/latest"
BACKEND_URL="{backend_url}"

echo "Agent Portal Installer"
echo "============================"
echo ""

# Create config directory
mkdir -p "${{CONFIG_DIR}}"

# Check if binary already exists (has auto-update, skip download)
if [ -x "${{BIN_PATH}}" ]; then
    echo "agent-portal already installed at: ${{BIN_PATH}}"
    echo "Skipping download (launcher has auto-update built in)"
    echo ""
else
    # Detect OS and architecture
    OS="$(uname -s)"
    ARCH="$(uname -m)"

    case "${{OS}}" in
        Linux)
            case "${{ARCH}}" in
                x86_64|amd64)
                    BINARY_NAME="agent-portal-linux-x86_64"
                    ;;
                aarch64|arm64)
                    BINARY_NAME="agent-portal-linux-aarch64"
                    ;;
                *)
                    echo "Error: Unsupported Linux architecture: ${{ARCH}}"
                    echo "Supported: x86_64, aarch64"
                    exit 1
                    ;;
            esac
            ;;
        Darwin)
            case "${{ARCH}}" in
                arm64|aarch64)
                    BINARY_NAME="agent-portal-darwin-aarch64"
                    ;;
                x86_64)
                    BINARY_NAME="agent-portal-darwin-x86_64"
                    ;;
                *)
                    echo "Error: Unsupported macOS architecture: ${{ARCH}}"
                    echo "Supported: arm64 (Apple Silicon), x86_64 (Intel)"
                    exit 1
                    ;;
            esac
            ;;
        *)
            echo "Error: Unsupported operating system: ${{OS}}"
            echo "Supported: Linux, Darwin (macOS)"
            echo "For Windows, download manually from:"
            echo "  ${{GITHUB_RELEASE_URL}}/agent-portal-windows-x86_64.exe"
            exit 1
            ;;
    esac

    DOWNLOAD_URL="${{GITHUB_RELEASE_URL}}/${{BINARY_NAME}}"

    echo "Detected: ${{OS}} ${{ARCH}}"
    echo "Binary: ${{BINARY_NAME}}"
    echo "Installing to: ${{BIN_PATH}}"
    echo ""

    # Download the binary to a temp file first (allows replacing running binary)
    TEMP_BIN="${{BIN_PATH}}.new.$$"
    echo "Downloading agent-portal from GitHub releases..."
    if command -v curl &> /dev/null; then
        curl -fsSL "${{DOWNLOAD_URL}}" -o "${{TEMP_BIN}}"
    elif command -v wget &> /dev/null; then
        wget -q "${{DOWNLOAD_URL}}" -O "${{TEMP_BIN}}"
    else
        echo "Error: curl or wget required"
        exit 1
    fi

    # Make executable
    chmod +x "${{TEMP_BIN}}"

    # Atomic replace (works even if binary is running)
    mv -f "${{TEMP_BIN}}" "${{BIN_PATH}}"

    # macOS: Remove quarantine attribute if present
    if [ "${{OS}}" = "Darwin" ]; then
        xattr -d com.apple.quarantine "${{BIN_PATH}}" 2>/dev/null || true
    fi

    echo "Binary installed successfully!"
    echo ""
fi

# Write config with backend URL (preserve existing config if present)
if [ -f "${{CONFIG_FILE}}" ]; then
    # Config exists — update backend_url if not already set
    if ! grep -q "backend_url" "${{CONFIG_FILE}}" 2>/dev/null; then
        echo "backend_url = \"${{BACKEND_URL}}\"" >> "${{CONFIG_FILE}}"
        echo "Added backend_url to existing config"
    else
        echo "Config already exists with backend_url, skipping"
    fi
else
    echo "Configuring backend URL: ${{BACKEND_URL}}"
    cat > "${{CONFIG_FILE}}" << EOF
backend_url = "${{BACKEND_URL}}"
EOF
fi
echo ""

# Add to PATH in shell rc files
add_to_path() {{
    local rc_file="$1"
    local path_line="export PATH=\"\$PATH:${{CONFIG_DIR}}\""

    if [ -f "${{rc_file}}" ]; then
        if ! grep -q "claude-portal" "${{rc_file}}" 2>/dev/null; then
            echo "" >> "${{rc_file}}"
            echo "# Agent Launcher binary path" >> "${{rc_file}}"
            echo "${{path_line}}" >> "${{rc_file}}"
            echo "Updated: ${{rc_file}}"
            return 0
        fi
    fi
    return 1
}}

echo "Adding to PATH..."

# Try common shell rc files
UPDATED_FILE=""
if add_to_path "${{HOME}}/.bashrc"; then UPDATED_FILE="${{HOME}}/.bashrc"; fi
if add_to_path "${{HOME}}/.zshrc"; then UPDATED_FILE="${{HOME}}/.zshrc"; fi
if add_to_path "${{HOME}}/.profile"; then UPDATED_FILE="${{HOME}}/.profile"; fi

if [ -n "${{UPDATED_FILE}}" ]; then
    echo ""
    echo "PATH updated! Restart your shell or run: source ${{UPDATED_FILE}}"
else
    echo "PATH already configured or no rc files found."
fi

echo ""

echo "Installation complete!"
echo ""
echo "Next steps:"
echo "  1. Log in:             agent-portal login"
echo "  2. Install as service: agent-portal service install"
echo ""
echo "Or run directly:         agent-portal"
"##
    );

    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/x-shellscript")
        .header(
            header::CONTENT_DISPOSITION,
            "attachment; filename=\"install.sh\"",
        )
        .body(Body::from(script))
        .unwrap()
}

/// Resolve the path to the portal binary
fn resolve_binary_path(dev_mode: bool) -> Result<std::path::PathBuf, (StatusCode, String)> {
    if dev_mode {
        // Try release first, then debug
        let release_path = std::path::Path::new("target/release/claude-portal");
        let debug_path = std::path::Path::new("target/debug/claude-portal");

        if release_path.exists() {
            Ok(release_path.to_path_buf())
        } else if debug_path.exists() {
            Ok(debug_path.to_path_buf())
        } else {
            Err((
                StatusCode::NOT_FOUND,
                "Portal binary not found. Run 'cargo build -p claude-portal --release' first."
                    .to_string(),
            ))
        }
    } else {
        // Production: use env var or default location
        let path = std::env::var("PROXY_BINARY_PATH")
            .unwrap_or_else(|_| "/app/bin/claude-portal".to_string());
        let path = std::path::PathBuf::from(path);

        if !path.exists() {
            Err((
                StatusCode::NOT_FOUND,
                format!("Portal binary not found at: {:?}", path),
            ))
        } else {
            Ok(path)
        }
    }
}

/// Compute SHA256 hash of a file
async fn compute_binary_sha256(path: &std::path::Path) -> Result<String, (StatusCode, String)> {
    let bytes = tokio::fs::read(path).await.map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Failed to read binary: {}", e),
        )
    })?;

    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    let hash = hasher.finalize();
    Ok(hex::encode(hash))
}

/// Serve the portal binary (GET) or return hash info (HEAD)
///
/// GET: Returns the binary file with X-Binary-SHA256 header
/// HEAD: Returns empty body with X-Binary-SHA256 header (for update checks)
pub async fn proxy_binary(
    method: Method,
    State(app_state): State<Arc<AppState>>,
) -> Result<Response<Body>, (StatusCode, String)> {
    let binary_path = resolve_binary_path(app_state.dev_mode)?;
    let sha256_hash = compute_binary_sha256(&binary_path).await?;

    if method == Method::HEAD {
        // HEAD request: return just headers for update check
        let metadata = tokio::fs::metadata(&binary_path).await.map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to read metadata: {}", e),
            )
        })?;

        Ok(Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "application/octet-stream")
            .header(header::CONTENT_LENGTH, metadata.len())
            .header("X-Binary-SHA256", &sha256_hash)
            .body(Body::empty())
            .unwrap())
    } else {
        // GET request: return the binary with hash header
        let file = tokio::fs::File::open(&binary_path).await.map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to open binary: {}", e),
            )
        })?;

        let stream = ReaderStream::new(file);
        let body = Body::from_stream(stream);

        Ok(Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "application/octet-stream")
            .header(
                header::CONTENT_DISPOSITION,
                "attachment; filename=\"claude-portal\"",
            )
            .header("X-Binary-SHA256", &sha256_hash)
            .body(body)
            .unwrap())
    }
}
