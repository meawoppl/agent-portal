# =============================================================================
# Dockerfile for Claude Code Portal Backend
# Uses pre-built binaries from CI (faster builds with caching)
# =============================================================================

FROM debian:bookworm-slim

WORKDIR /app

# Install runtime dependencies
RUN apt-get update && \
    apt-get install -y \
    ca-certificates \
    libpq5 \
    libssl3 \
    curl \
    && rm -rf /var/lib/apt/lists/*

# Copy pre-built backend binary from CI (frontend assets are embedded)
COPY build-output/backend /app/backend

# Create non-root user
RUN useradd -m -u 1001 -s /bin/bash appuser && \
    chown -R appuser:appuser /app

USER appuser

# Expose port
EXPOSE 3000

# =============================================================================
# Environment variable reference (pass via -e or docker-compose environment:)
# =============================================================================
#
# Required:
#   DATABASE_URL          PostgreSQL connection string
#                         e.g. postgresql://user:pass@host:5432/dbname
#
# Authentication (required in production, omit for --dev-mode):
#   GOOGLE_CLIENT_ID      Google OAuth client ID
#   GOOGLE_CLIENT_SECRET  Google OAuth client secret
#   GOOGLE_REDIRECT_URI   OAuth callback URL (e.g. https://example.com/api/auth/callback)
#   SESSION_SECRET        Secret key for signing session cookies (min 32 chars)
#
# Optional:
#   APP_TITLE             Text shown in the dashboard header (default: "Agent Portal")
#   ALLOWED_EMAIL_DOMAIN  Restrict login to one email domain (e.g. "company.com")
#   ALLOWED_EMAILS        Comma-separated list of allowed emails (e.g. "a@b.com,c@d.com")
#   HOST                  Bind address (default: 0.0.0.0)
#   PORT                  Listen port (default: 3000)
#   RUST_LOG              Log level, e.g. "info" or "debug" (default: "info")
# =============================================================================

# Health check
HEALTHCHECK --interval=30s --timeout=3s --start-period=5s --retries=3 \
    CMD curl -f http://localhost:3000/ || exit 1

# Run the backend (expects environment variables to be passed in)
CMD ["/app/backend"]
