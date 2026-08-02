//! Environment configuration, shared across providers.
//!
//! Every provider needs a key; beyond that they diverge — Azure and AWS are
//! regional, Google authenticates with a service-account file rather than a
//! key, IBM issues a per-instance URL, AWS needs a bucket to stage audio in.
//! Rather than invent a variable per provider per field, there is one variable
//! per *concept* and each provider declares what it requires, so a missing
//! value names the provider that wanted it.

pub const BACKEND_VAR: &str = "PORTAL_STT_BACKEND";
pub const API_KEY_VAR: &str = "PORTAL_STT_API_KEY";
pub const MODEL_VAR: &str = "PORTAL_STT_MODEL";
pub const ENDPOINT_VAR: &str = "PORTAL_STT_ENDPOINT";
pub const REGION_VAR: &str = "PORTAL_STT_REGION";
pub const LANGUAGE_VAR: &str = "PORTAL_STT_LANGUAGE";
pub const BUCKET_VAR: &str = "PORTAL_STT_BUCKET";
pub const SERVICE_ACCOUNT_VAR: &str = "PORTAL_STT_SERVICE_ACCOUNT_PATH";
pub const VOCABULARY_VAR: &str = "PORTAL_STT_VOCABULARY_NAME";

/// Every STT-related variable, read once at boot.
#[derive(Debug, Default, Clone)]
pub struct SttEnv {
    pub api_key: Option<String>,
    pub model: Option<String>,
    pub endpoint: Option<String>,
    pub region: Option<String>,
    pub language: Option<String>,
    pub bucket: Option<String>,
    pub service_account_path: Option<String>,
    pub vocabulary_name: Option<String>,
}

impl SttEnv {
    pub fn from_process() -> Self {
        let read = |name: &str| {
            std::env::var(name)
                .ok()
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
        };
        Self {
            api_key: read(API_KEY_VAR),
            model: read(MODEL_VAR),
            endpoint: read(ENDPOINT_VAR),
            region: read(REGION_VAR),
            language: read(LANGUAGE_VAR),
            bucket: read(BUCKET_VAR),
            service_account_path: read(SERVICE_ACCOUNT_VAR),
            vocabulary_name: read(VOCABULARY_VAR),
        }
    }

    /// A required value, or an error naming both the variable and the provider
    /// that needs it — the two things an operator needs to fix it.
    pub fn require(&self, field: Field, backend: &str) -> anyhow::Result<String> {
        self.get(field).ok_or_else(|| {
            anyhow::anyhow!(
                "{BACKEND_VAR}={backend} requires {} to be set",
                field.var_name()
            )
        })
    }

    pub fn get(&self, field: Field) -> Option<String> {
        match field {
            Field::ApiKey => self.api_key.clone(),
            Field::Model => self.model.clone(),
            Field::Endpoint => self.endpoint.clone(),
            Field::Region => self.region.clone(),
            Field::Language => self.language.clone(),
            Field::Bucket => self.bucket.clone(),
            Field::ServiceAccountPath => self.service_account_path.clone(),
            Field::VocabularyName => self.vocabulary_name.clone(),
        }
    }

    /// The configured model, or the provider's default.
    pub fn model_or(&self, default: &str) -> String {
        self.model.clone().unwrap_or_else(|| default.to_string())
    }

    /// The configured endpoint, or the provider's default.
    pub fn endpoint_or(&self, default: &str) -> String {
        self.endpoint
            .clone()
            .unwrap_or_else(|| default.to_string())
            .trim_end_matches('/')
            .to_string()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    ApiKey,
    Model,
    Endpoint,
    Region,
    Language,
    Bucket,
    ServiceAccountPath,
    VocabularyName,
}

impl Field {
    pub fn var_name(self) -> &'static str {
        match self {
            Self::ApiKey => API_KEY_VAR,
            Self::Model => MODEL_VAR,
            Self::Endpoint => ENDPOINT_VAR,
            Self::Region => REGION_VAR,
            Self::Language => LANGUAGE_VAR,
            Self::Bucket => BUCKET_VAR,
            Self::ServiceAccountPath => SERVICE_ACCOUNT_VAR,
            Self::VocabularyName => VOCABULARY_VAR,
        }
    }
}

/// The language a request should use: the caller's, else the deploy default,
/// else the supplied fallback. Providers that *require* a language (Google,
/// Azure) need the fallback; the rest pass `None` through as auto-detect.
pub fn resolve_language(
    request_language: Option<&str>,
    configured: Option<&str>,
    fallback: &str,
) -> String {
    request_language
        .or(configured)
        .unwrap_or(fallback)
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_requirement_names_the_variable_and_the_provider() {
        let env = SttEnv::default();
        let err = env.require(Field::Region, "azure").unwrap_err().to_string();
        assert!(err.contains(REGION_VAR), "{err}");
        assert!(err.contains("azure"), "{err}");
    }

    #[test]
    fn model_and_endpoint_fall_back_to_provider_defaults() {
        let env = SttEnv::default();
        assert_eq!(env.model_or("nova-3"), "nova-3");
        assert_eq!(env.endpoint_or("https://x.invalid"), "https://x.invalid");

        let configured = SttEnv {
            model: Some("whisper-1".into()),
            endpoint: Some("https://y.invalid/".into()),
            ..Default::default()
        };
        assert_eq!(configured.model_or("nova-3"), "whisper-1");
        // Trailing slash stripped so providers can append paths freely.
        assert_eq!(
            configured.endpoint_or("https://x.invalid"),
            "https://y.invalid"
        );
    }

    #[test]
    fn request_language_wins_over_deploy_default_which_wins_over_fallback() {
        assert_eq!(
            resolve_language(Some("fr-FR"), Some("de-DE"), "en-US"),
            "fr-FR"
        );
        assert_eq!(resolve_language(None, Some("de-DE"), "en-US"), "de-DE");
        assert_eq!(resolve_language(None, None, "en-US"), "en-US");
    }
}
