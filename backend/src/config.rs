//! Runtime configuration, read once at startup from `PICWEIGHT_*` env vars.
//!
//! Everything the process needs lives here; nothing else reads `std::env`.
//! Missing *required* values fail fast with a message naming the variable —
//! a misconfigured pod should crash-loop with a readable reason, not start and
//! 500 on the first request.
//!
//! | Variable | Required | Default |
//! |---|---|---|
//! | `PICWEIGHT_PORT` | no | `33100` (phos holds 33000) |
//! | `PICWEIGHT_DATA_PATH` | no | `./data` |
//! | `PICWEIGHT_STATIC_DIR` | no | `./static` |
//! | `PICWEIGHT_DATABASE_URL` | no | `<data_path>/picweight.db` |
//! | `PICWEIGHT_OIDC_ISSUER` | **yes** | — |
//! | `PICWEIGHT_OIDC_CLIENT_ID` | **yes** | — |
//! | `PICWEIGHT_OIDC_CLIENT_SECRET` | **yes** | — |
//! | `PICWEIGHT_OIDC_MOBILE_CLIENT_ID` | no | none (web client only) |
//! | `PICWEIGHT_OIDC_REDIRECT_URI` | no | `http://localhost:<port>/api/auth/callback` |
//! | `PICWEIGHT_OIDC_SCOPES` | no | `openid profile email` |
//! | `PICWEIGHT_OIDC_EXTRA_AUDIENCES` | no | empty = accept any extra `aud`, logged (Zitadel appends its project id) |
//! | `PICWEIGHT_JWT_SECRET` | no | generated once into `<data_path>/.picweight_jwt_secret` |
//! | `PICWEIGHT_JWT_TTL` | no | `1209600` (14 days) |
//! | `PICWEIGHT_OPENAI_API_KEY` | **yes** | — (PRD §4: working without a key is a non-goal) |
//! | `PICWEIGHT_OPENAI_MODEL` | no | `gpt-5.4-mini` |
//! | `PICWEIGHT_OPENAI_BASE_URL` | no | `https://api.openai.com/v1` |
//! | `PICWEIGHT_RUST_LOG` / `RUST_LOG` | no | `info` |
//! | `PICWEIGHT_WEB_SEARCH_ENABLED` | no | `false` (§5: config-gated, off by default) |

use std::path::{Path, PathBuf};

/// Default listen port. phos holds 33000, picweight takes the next slot.
pub const DEFAULT_PORT: u16 = 33100;
/// Default session lifetime: 14 days, matching phos.
pub const DEFAULT_JWT_TTL_SECS: u64 = 1_209_600;
/// Default vision model. Overridable so a model swap needs no rebuild.
///
/// Must accept image input, tool calls and strict structured output — the
/// estimation agent (PRD §5) uses all three, and a model missing any of them
/// fails at the first photo rather than at startup.
///
/// Kept in step with `helm/picweight/values.yaml`'s `openai.model`, which sets
/// the env var explicitly on every deployment. This constant only governs a bare
/// `cargo run` with no `PICWEIGHT_OPENAI_MODEL` set.
pub const DEFAULT_OPENAI_MODEL: &str = "gpt-5.4-mini";
/// Default OpenAI API root, with no trailing slash.
///
/// Overridable for three reasons, in increasing order of how often they bite:
/// an OpenAI-compatible proxy (LiteLLM, vLLM, Azure), and the PRD §13
/// integration tests, which point both the agent loop and the verdict call at a
/// local mock rather than the real provider.
pub const DEFAULT_OPENAI_BASE_URL: &str = "https://api.openai.com/v1";
/// Default OIDC scopes.
pub const DEFAULT_SCOPES: &str = "openid profile email";
/// File under `data_path` holding the generated JWT secret.
pub const JWT_SECRET_FILE: &str = ".picweight_jwt_secret";
/// Sub-directory of `data_path` holding the content-addressed thumbnails.
pub const THUMBS_DIR: &str = "thumbs";

/// Why configuration could not be loaded.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// A required variable is absent or empty.
    #[error("{0} is required but not set")]
    Missing(&'static str),
    /// A variable is present but unparseable.
    #[error("{name} has an invalid value {value:?}: {reason}")]
    Invalid {
        /// Variable name.
        name: &'static str,
        /// The offending value.
        value: String,
        /// Why it was rejected.
        reason: String,
    },
    /// The data directory could not be created or written to.
    #[error("data path {path} is unusable: {source}")]
    DataPath {
        /// The path that failed.
        path: String,
        /// Underlying IO error.
        #[source]
        source: std::io::Error,
    },
}

/// OIDC settings. Two clients as in phos: a confidential **web** client
/// (`client_id` + `client_secret`) and an optional public/native **mobile**
/// client (`mobile_client_id`, PKCE, no secret).
#[derive(Debug, Clone)]
pub struct OidcConfig {
    /// Issuer URL used for discovery.
    pub issuer: String,
    /// Confidential web client id.
    pub client_id: String,
    /// Confidential web client secret.
    pub client_secret: String,
    /// Public/native client id used by the Android app, if configured.
    pub mobile_client_id: Option<String>,
    /// Redirect URI registered for the web client.
    pub redirect_uri: String,
    /// Scopes requested at authorization time.
    pub scopes: Vec<String>,
    /// Additional `aud` values accepted on ID tokens beyond our own client ids.
    ///
    /// OIDC Core §3.1.3.7 requires `aud` to *contain* this client's id — that
    /// check is enforced separately and is the security-critical one. Providers
    /// are explicitly permitted to list further audiences alongside it, and
    /// several do: **Zitadel appends the numeric project id to every ID token**,
    /// so a real token arrives as `aud = [<client_id>@<project>, <project_id>]`.
    ///
    /// Empty (the default) means *accept any additional audience*, logging each
    /// unrecognised value once at WARN so it can be pasted into
    /// `PICWEIGHT_OIDC_EXTRA_AUDIENCES` to tighten this to an allowlist.
    /// Non-empty switches to strict allowlist mode.
    pub extra_audiences: Vec<String>,
}

/// Fully resolved runtime configuration.
#[derive(Debug, Clone)]
pub struct Config {
    /// TCP port to listen on.
    pub port: u16,
    /// Writable directory holding the SQLite database and `thumbs/`.
    pub data_path: PathBuf,
    /// Directory served by the SPA fallback (built frontend + APK).
    pub static_dir: PathBuf,
    /// SQLite file path handed to diesel.
    pub database_url: String,
    /// OIDC provider + client settings.
    pub oidc: OidcConfig,
    /// HMAC secret for the session JWTs we mint.
    pub jwt_secret: String,
    /// Session lifetime in seconds.
    pub jwt_ttl_secs: u64,
    /// OpenAI API key. Lives only here — clients never see it (§7).
    pub openai_api_key: String,
    /// Model id passed to rig.
    pub openai_model: String,
    /// API root for every OpenAI call — the rig agent *and* the verdict
    /// completion. Never has a trailing slash.
    pub openai_base_url: String,
    /// `tracing-subscriber` env filter directive.
    pub rust_log: String,
    /// Whether the agent's `web_search` tool is registered (§5, off by default).
    pub web_search_enabled: bool,
}

impl Config {
    /// Read and validate configuration from the process environment.
    ///
    /// Creates `data_path` (and `data_path/thumbs`) if absent, and generates a
    /// persistent JWT secret on first run when `PICWEIGHT_JWT_SECRET` is unset.
    pub fn from_env() -> Result<Self, ConfigError> {
        let port = parse_var("PICWEIGHT_PORT", DEFAULT_PORT)?;

        let data_path = PathBuf::from(
            optional("PICWEIGHT_DATA_PATH").unwrap_or_else(|| "./data".to_string()),
        );
        std::fs::create_dir_all(data_path.join(THUMBS_DIR)).map_err(|source| {
            ConfigError::DataPath {
                path: data_path.display().to_string(),
                source,
            }
        })?;

        let static_dir = PathBuf::from(
            optional("PICWEIGHT_STATIC_DIR").unwrap_or_else(|| "./static".to_string()),
        );

        let database_url = optional("PICWEIGHT_DATABASE_URL").unwrap_or_else(|| {
            data_path
                .join("picweight.db")
                .to_string_lossy()
                .into_owned()
        });

        let issuer = required("PICWEIGHT_OIDC_ISSUER")?;
        let client_id = required("PICWEIGHT_OIDC_CLIENT_ID")?;
        let client_secret = required("PICWEIGHT_OIDC_CLIENT_SECRET")?;
        let mobile_client_id = optional("PICWEIGHT_OIDC_MOBILE_CLIENT_ID");
        let redirect_uri = optional("PICWEIGHT_OIDC_REDIRECT_URI")
            .unwrap_or_else(|| format!("http://localhost:{port}/api/auth/callback"));
        let scopes = optional("PICWEIGHT_OIDC_SCOPES")
            .unwrap_or_else(|| DEFAULT_SCOPES.to_string())
            .split_whitespace()
            .map(str::to_string)
            .collect::<Vec<_>>();
        let extra_audiences = optional("PICWEIGHT_OIDC_EXTRA_AUDIENCES")
            .unwrap_or_default()
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect::<Vec<_>>();

        let jwt_secret = match optional("PICWEIGHT_JWT_SECRET") {
            Some(secret) => secret,
            None => load_or_create_jwt_secret(&data_path)?,
        };
        let jwt_ttl_secs = parse_var("PICWEIGHT_JWT_TTL", DEFAULT_JWT_TTL_SECS)?;

        // Non-goal (§4): "working without an OpenAI API key". Fail at startup
        // rather than at the first photo.
        let openai_api_key = required("PICWEIGHT_OPENAI_API_KEY")?;
        let openai_model =
            optional("PICWEIGHT_OPENAI_MODEL").unwrap_or_else(|| DEFAULT_OPENAI_MODEL.to_string());
        let openai_base_url = normalize_base_url(
            optional("PICWEIGHT_OPENAI_BASE_URL")
                .unwrap_or_else(|| DEFAULT_OPENAI_BASE_URL.to_string()),
        );

        let rust_log = optional("PICWEIGHT_RUST_LOG")
            .or_else(|| optional("RUST_LOG"))
            .unwrap_or_else(|| "info".to_string());

        let web_search_enabled = parse_bool("PICWEIGHT_WEB_SEARCH_ENABLED", false)?;

        Ok(Config {
            port,
            data_path,
            static_dir,
            database_url,
            oidc: OidcConfig {
                issuer,
                client_id,
                client_secret,
                mobile_client_id,
                redirect_uri,
                scopes,
                extra_audiences,
            },
            jwt_secret,
            jwt_ttl_secs,
            openai_api_key,
            openai_model,
            openai_base_url,
            rust_log,
            web_search_enabled,
        })
    }

    /// Chat-completions endpoint derived from [`Config::openai_base_url`].
    ///
    /// Used by the verdict call in [`crate::feedback::phrasing`], which is a
    /// single short completion and deliberately does not go through rig.
    pub fn chat_completions_url(&self) -> String {
        format!("{}/chat/completions", self.openai_base_url)
    }

    /// Absolute path of the content-addressed thumbnail root.
    pub fn thumbs_root(&self) -> PathBuf {
        self.data_path.join(THUMBS_DIR)
    }

    /// `index.html` of the SPA, used as the `fallback_service` not-found target.
    pub fn spa_index(&self) -> PathBuf {
        self.static_dir.join("index.html")
    }
}

/// Strip trailing slashes so callers can concatenate a path unconditionally.
fn normalize_base_url(raw: String) -> String {
    let trimmed = raw.trim().trim_end_matches('/');
    if trimmed.is_empty() {
        DEFAULT_OPENAI_BASE_URL.to_string()
    } else {
        trimmed.to_string()
    }
}

/// Read a variable that must be present and non-empty.
fn required(name: &'static str) -> Result<String, ConfigError> {
    optional(name).ok_or(ConfigError::Missing(name))
}

/// Read a variable, treating empty strings as absent.
fn optional(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.trim().is_empty())
}

/// Read and parse a variable, falling back to `default` when unset.
fn parse_var<T>(name: &'static str, default: T) -> Result<T, ConfigError>
where
    T: std::str::FromStr,
    T::Err: std::fmt::Display,
{
    match optional(name) {
        None => Ok(default),
        Some(raw) => raw.trim().parse::<T>().map_err(|e| ConfigError::Invalid {
            name,
            value: raw,
            reason: e.to_string(),
        }),
    }
}

/// Read a boolean variable accepting `1/true/yes/on` and `0/false/no/off`.
fn parse_bool(name: &'static str, default: bool) -> Result<bool, ConfigError> {
    match optional(name) {
        None => Ok(default),
        Some(raw) => match raw.trim().to_ascii_lowercase().as_str() {
            "1" | "true" | "yes" | "on" => Ok(true),
            "0" | "false" | "no" | "off" => Ok(false),
            _ => Err(ConfigError::Invalid {
                name,
                value: raw,
                reason: "expected a boolean (true/false)".to_string(),
            }),
        },
    }
}

/// Read the persisted JWT secret, generating one on first run.
///
/// Ported from phos: keeps sessions alive across restarts without forcing the
/// operator to manage yet another secret when they do not care to.
fn load_or_create_jwt_secret(data_path: &Path) -> Result<String, ConfigError> {
    let secret_path = data_path.join(JWT_SECRET_FILE);
    if let Ok(existing) = std::fs::read_to_string(&secret_path) {
        let trimmed = existing.trim();
        if !trimmed.is_empty() {
            return Ok(trimmed.to_string());
        }
    }
    let secret = uuid::Uuid::new_v4().simple().to_string();
    std::fs::write(&secret_path, &secret).map_err(|source| ConfigError::DataPath {
        path: secret_path.display().to_string(),
        source,
    })?;
    tracing::info!("generated a new JWT secret at {}", secret_path.display());
    Ok(secret)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_required_names_the_variable() {
        let err = required("PICWEIGHT_DEFINITELY_UNSET_VARIABLE_FOR_TESTS").unwrap_err();
        assert!(err
            .to_string()
            .contains("PICWEIGHT_DEFINITELY_UNSET_VARIABLE_FOR_TESTS"));
    }

    #[test]
    fn a_base_url_never_keeps_a_trailing_slash() {
        assert_eq!(
            normalize_base_url("http://127.0.0.1:9/v1/".into()),
            "http://127.0.0.1:9/v1"
        );
        assert_eq!(
            normalize_base_url("http://127.0.0.1:9/v1///".into()),
            "http://127.0.0.1:9/v1"
        );
        assert_eq!(
            normalize_base_url("  http://127.0.0.1:9/v1  ".into()),
            "http://127.0.0.1:9/v1"
        );
    }

    #[test]
    fn a_blank_base_url_falls_back_to_the_provider() {
        assert_eq!(normalize_base_url("   ".into()), DEFAULT_OPENAI_BASE_URL);
        assert_eq!(normalize_base_url("///".into()), DEFAULT_OPENAI_BASE_URL);
    }

    #[test]
    fn jwt_secret_is_stable_across_calls() {
        let dir = tempfile::tempdir().unwrap();
        let first = load_or_create_jwt_secret(dir.path()).unwrap();
        let second = load_or_create_jwt_secret(dir.path()).unwrap();
        assert_eq!(first, second);
        assert!(!first.is_empty());
    }
}
