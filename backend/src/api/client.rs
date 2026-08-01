//! `GET /api/v1/client/version` — what the self-hosted Android client needs to
//! update itself in place.
//!
//! The image bundles the APK it was built with at `static/picweight.apk`, and
//! the `android-builder` Docker stage writes a sidecar next to it describing
//! that exact file:
//!
//! ```json
//! {"version_name":"1.2.3","version_code":12,"sha256":"d9b9…6349","size_bytes":25801464}
//! ```
//!
//! This module serves that sidecar. The app compares its own
//! `BuildConfig.VERSION_CODE` against [`ClientVersionResponse::version_code`],
//! offers an update only when the server's is **strictly greater**, downloads
//! [`ClientVersionResponse::download_path`], and verifies the bytes against
//! [`ClientVersionResponse::sha256`] before handing them to `PackageInstaller`.
//!
//! # Why the metadata is read from disk rather than from `BuildConfig`
//!
//! The backend cannot introspect the APK — it is an opaque blob in the same
//! image. Taking `version_code`/`version_name` from the same values the Docker
//! stage passed to Gradle is what makes the advertised version agree with what
//! is actually inside the file; the stage then re-reads the built APK with
//! `aapt2 dump badging` and fails the build if they ever disagree.
//!
//! # Why a missing APK is not an error
//!
//! A backend-only build, a `cargo run` on a laptop, and every integration test
//! have no APK and no sidecar. That is a normal state, not a fault, so it is
//! reported as a well-formed "nothing to offer" payload — see
//! [`client_version`].

use std::path::Path;

use crate::AppState;
use axum::extract::State;
use axum::Json;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// File name of the APK the image bundles, relative to the static directory.
pub const APK_FILE_NAME: &str = "picweight.apk";

/// Sidecar written next to the APK by the Dockerfile's `android-builder` stage.
pub const APK_METADATA_FILE_NAME: &str = "picweight.apk.json";

/// Where the APK downloads from, relative to the server root.
///
/// Published in every response so the client never hardcodes it: the day this
/// moves, the app follows without a release.
pub const APK_DOWNLOAD_PATH: &str = "/picweight.apk";

/// `version_code` advertised when there is nothing to offer.
///
/// Zero, because a real Android `versionCode` starts at 1 and the client only
/// updates on *strictly greater* — so this value can never trigger an update,
/// no matter how old the installed build is.
pub const NO_RELEASE_VERSION_CODE: i32 = 0;

/// The sidecar as written by the Docker build.
///
/// Deliberately separate from [`ClientVersionResponse`]: this is a file format
/// produced by a shell script in another stage, and it is parsed defensively.
/// Unknown keys are tolerated so the build can add fields without breaking a
/// running server.
#[derive(Debug, Clone, Deserialize)]
struct ApkSidecar {
    /// `versionName` the APK was built with.
    version_name: String,
    /// `versionCode` the APK was built with — the git commit count.
    version_code: i32,
    /// Lowercase hex SHA-256 of the APK bytes.
    sha256: String,
    /// Size of the APK in bytes.
    size_bytes: i64,
}

/// `GET /api/v1/client/version` response — the bundled Android build.
///
/// Every field is non-nullable, so a generated client needs no null-handling at
/// all: "no APK bundled" is expressed by `available: false` with
/// `version_code: 0`, not by absent fields and not by an error status.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct ClientVersionResponse {
    /// Whether this server has an APK to offer at all.
    ///
    /// `false` for a backend-only build, for local development, and whenever
    /// the bundled metadata is unusable. A client that only compares version
    /// codes does not need to read this — it exists so a "check for updates"
    /// screen can say "this server does not ship an APK" instead of the
    /// misleading "you are up to date".
    pub available: bool,
    /// `versionName` of the bundled APK — a tag (`1.2.3`) or `master+<sha>`.
    ///
    /// Empty when `available` is `false`. Display only: version *names* are not
    /// ordered and must never be compared.
    pub version_name: String,
    /// `versionCode` of the bundled APK: the git commit count, monotonic on
    /// master.
    ///
    /// `0` when `available` is `false`. This is the only field an update
    /// decision may be based on, and only as
    /// `version_code > BuildConfig.VERSION_CODE` — never `!=`, which would
    /// offer a downgrade after a rollback.
    pub version_code: i32,
    /// Lowercase hex SHA-256 of the APK at `download_path`.
    ///
    /// Empty when `available` is `false`. The client must verify the downloaded
    /// bytes against this before handing them to the installer.
    pub sha256: String,
    /// Size of the APK in bytes, so a client can show download progress and
    /// reject an obviously truncated response early. `0` when unavailable.
    pub size_bytes: i64,
    /// Server-relative path the APK downloads from, always populated so the
    /// client never hardcodes it.
    pub download_path: String,
}

impl ClientVersionResponse {
    /// The "nothing to offer" payload.
    ///
    /// Still carries [`APK_DOWNLOAD_PATH`], because the field describes where
    /// the APK *would* live and clients should not have to special-case it.
    pub fn none() -> Self {
        ClientVersionResponse {
            available: false,
            version_name: String::new(),
            version_code: NO_RELEASE_VERSION_CODE,
            sha256: String::new(),
            size_bytes: 0,
            download_path: APK_DOWNLOAD_PATH.to_string(),
        }
    }
}

/// Read and validate the bundled-APK metadata once, at startup.
///
/// Called from [`AppState::new`](crate::AppState::new); the result is cached
/// for the life of the process because neither the APK nor its sidecar can
/// change without a new image. **Never fails** — every problem degrades to
/// [`ClientVersionResponse::none`] with a log line naming the reason, so a
/// broken sidecar can never stop the server from starting.
pub fn load_bundled_release(static_dir: &Path) -> ClientVersionResponse {
    let metadata_path = static_dir.join(APK_METADATA_FILE_NAME);
    let raw = match std::fs::read_to_string(&metadata_path) {
        Ok(raw) => raw,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            // The common case for a backend-only build or a laptop. Not a
            // warning: there is nothing wrong.
            tracing::info!(
                path = %metadata_path.display(),
                "no bundled APK metadata; /api/v1/client/version will report none"
            );
            return ClientVersionResponse::none();
        }
        Err(err) => {
            tracing::warn!(
                path = %metadata_path.display(),
                error = %err,
                "bundled APK metadata is unreadable; reporting no client update"
            );
            return ClientVersionResponse::none();
        }
    };

    match parse_sidecar(&raw, static_dir) {
        Ok(release) => {
            tracing::info!(
                version_name = %release.version_name,
                version_code = release.version_code,
                size_bytes = release.size_bytes,
                "bundled Android client ready for in-app update"
            );
            release
        }
        Err(reason) => {
            // Advertising an APK we cannot describe correctly is worse than
            // advertising none: the client verifies the download against
            // `sha256` and would reject every attempt as corrupt, on every
            // launch, with no way for the user to make progress.
            tracing::warn!(
                path = %metadata_path.display(),
                reason = %reason,
                "bundled APK metadata is unusable; reporting no client update"
            );
            ClientVersionResponse::none()
        }
    }
}

/// Parse one sidecar and cross-check it against the APK sitting next to it.
///
/// The APK checks are not paranoia about the Docker build — they catch the
/// SPA-fallback footgun. `static_dir` is served with an `index.html` fallback,
/// so a *missing* `picweight.apk` does not 404: it returns the SPA shell with
/// `200 text/html`. A client told to download it would fetch HTML, fail the
/// digest check, and report a corrupt download forever.
fn parse_sidecar(raw: &str, static_dir: &Path) -> Result<ClientVersionResponse, String> {
    let sidecar: ApkSidecar =
        serde_json::from_str(raw).map_err(|err| format!("not valid sidecar JSON: {err}"))?;

    let version_name = sidecar.version_name.trim();
    if version_name.is_empty() {
        return Err("version_name is empty".to_string());
    }
    if sidecar.version_code <= 0 {
        return Err(format!(
            "version_code {} is not a positive integer, so no client could ever \
be older than it",
            sidecar.version_code
        ));
    }
    if !is_sha256_hex(&sidecar.sha256) {
        return Err(format!(
            "sha256 {:?} is not 64 lowercase hex characters",
            sidecar.sha256
        ));
    }
    if sidecar.size_bytes <= 0 {
        return Err(format!("size_bytes {} is not positive", sidecar.size_bytes));
    }

    let apk_path = static_dir.join(APK_FILE_NAME);
    let on_disk = std::fs::metadata(&apk_path).map_err(|err| {
        format!(
            "metadata describes {} but that file is unreadable: {err}",
            apk_path.display()
        )
    })?;
    if on_disk.len() != sidecar.size_bytes as u64 {
        return Err(format!(
            "metadata says {} bytes but {} is {} bytes — the APK and its sidecar \
did not come from the same build",
            sidecar.size_bytes,
            apk_path.display(),
            on_disk.len()
        ));
    }

    Ok(ClientVersionResponse {
        available: true,
        version_name: version_name.to_string(),
        version_code: sidecar.version_code,
        sha256: sidecar.sha256,
        size_bytes: sidecar.size_bytes,
        download_path: APK_DOWNLOAD_PATH.to_string(),
    })
}

/// True for exactly 64 lowercase hex characters — the form `sha256sum` emits.
///
/// Case is part of the contract rather than a nicety: the client compares the
/// advertised digest against the one it computes, and a case-insensitive
/// comparison is the kind of thing that gets written as `==` and silently
/// rejects every download.
fn is_sha256_hex(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// `GET /api/v1/client/version` — metadata for the APK this server bundles.
///
/// # Unauthenticated, deliberately
///
/// This route sits **outside** [`require_auth`](crate::auth::require_auth),
/// alongside `/healthz`. That is not a leak: `/picweight.apk` is already public
/// — the web UI's download card links it and anyone can fetch the whole APK
/// without a session. This endpoint publishes strictly less than that download
/// already discloses (a version string, the digest of those same public bytes,
/// and their length), and nothing user-scoped. Requiring a session here would
/// also break the case that matters most, an app too old to hold a valid
/// session still needing to discover that a newer build exists.
///
/// # Always `200`, never a `404`
///
/// When no APK is bundled the response is [`ClientVersionResponse::none`] —
/// `available: false` with `version_code: 0` — rather than a `404` or a `503`.
/// Two reasons:
///
/// 1. `version_code: 0` is *already* the correct answer to the client's only
///    question. The update rule is `server > BuildConfig.VERSION_CODE`, and no
///    installed build has a version code below 1, so a backend-only server
///    reads as "up to date" through the exact same code path as a server whose
///    APK matches. There is no error branch to get wrong.
/// 2. The check runs unprompted on app start, where a non-2xx would surface as
///    a failed request. The app must never report itself broken, or offline,
///    because the server it is happily talking to does not happen to ship an
///    APK.
///
/// The same payload is returned when the sidecar exists but is malformed. That
/// is a server-side misbuild, logged at `warn` for the operator, and there is
/// nothing the user could do with a `500` except be blocked.
#[utoipa::path(
    get,
    path = "/api/v1/client/version",
    tag = "client",
    summary = "Bundled Android client version",
    description = "Metadata for the APK this server ships, so the Android app can \
offer an in-app update. Unauthenticated, like the /picweight.apk download it \
describes. Always 200: a server with no bundled APK answers `available: false` \
with `version_code: 0`, which the client reads as \"up to date\".",
    responses(
        (status = 200, description = "The bundled APK, or `available: false` when \
there is none", body = ClientVersionResponse),
    )
)]
pub async fn client_version(State(state): State<AppState>) -> Json<ClientVersionResponse> {
    // Resolved once at startup — this handler does no IO.
    Json(state.client_release.as_ref().clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// A static dir holding an APK of `size` bytes plus whatever sidecar text
    /// the test wants.
    fn static_dir(sidecar: Option<&str>, apk_bytes: Option<usize>) -> (tempfile::TempDir, PathBuf) {
        let tmp = tempfile::tempdir().expect("temp dir");
        let dir = tmp.path().to_path_buf();
        if let Some(text) = sidecar {
            std::fs::write(dir.join(APK_METADATA_FILE_NAME), text).expect("write sidecar");
        }
        if let Some(size) = apk_bytes {
            std::fs::write(dir.join(APK_FILE_NAME), vec![0u8; size]).expect("write apk");
        }
        (tmp, dir)
    }

    /// Byte-for-byte what the `android-builder` stage printed for a real build.
    const REAL_SIDECAR: &str = r#"{"version_name":"1.2.3","version_code":12,"sha256":"d9b9f652304678be031b55dfc0b0621941450d5fb398299fea155d84be926349","size_bytes":9}
"#;

    #[test]
    fn a_real_sidecar_becomes_the_advertised_release() {
        let (_tmp, dir) = static_dir(Some(REAL_SIDECAR), Some(9));
        let release = load_bundled_release(&dir);
        assert_eq!(
            release,
            ClientVersionResponse {
                available: true,
                version_name: "1.2.3".to_string(),
                version_code: 12,
                sha256: "d9b9f652304678be031b55dfc0b0621941450d5fb398299fea155d84be926349"
                    .to_string(),
                size_bytes: 9,
                download_path: "/picweight.apk".to_string(),
            }
        );
    }

    #[test]
    fn a_master_build_version_name_survives_intact() {
        // `master+<sha>` is what every non-tagged build now advertises; it is
        // not semver and must not be normalised into anything.
        let (_tmp, dir) = static_dir(
            Some(
                r#"{"version_name":"master+b5fcc95","version_code":1337,"sha256":"0000000000000000000000000000000000000000000000000000000000000000","size_bytes":4}"#,
            ),
            Some(4),
        );
        let release = load_bundled_release(&dir);
        assert!(release.available);
        assert_eq!(release.version_name, "master+b5fcc95");
        assert_eq!(release.version_code, 1337);
    }

    #[test]
    fn an_absent_sidecar_is_reported_as_no_release() {
        let (_tmp, dir) = static_dir(None, None);
        assert_eq!(load_bundled_release(&dir), ClientVersionResponse::none());
    }

    #[test]
    fn the_no_release_payload_can_never_trigger_an_update() {
        let none = ClientVersionResponse::none();
        assert!(!none.available);
        // The client's rule is `server > installed`; the lowest version code
        // Android will accept in a manifest is 1.
        assert!(none.version_code < 1);
        // Still tells the client where the APK lives, so nothing is hardcoded.
        assert_eq!(none.download_path, APK_DOWNLOAD_PATH);
    }

    #[test]
    fn malformed_metadata_degrades_instead_of_failing() {
        for (label, sidecar, apk_bytes) in [
            ("truncated json", r#"{"version_name":"1.2.3","#, Some(9)),
            ("not an object", r#"[1,2,3]"#, Some(9)),
            ("empty file", "", Some(9)),
            (
                "missing version_code",
                r#"{"version_name":"1.2.3","sha256":"d9b9f652304678be031b55dfc0b0621941450d5fb398299fea155d84be926349","size_bytes":9}"#,
                Some(9),
            ),
            (
                "version_code as a string",
                r#"{"version_name":"1.2.3","version_code":"12","sha256":"d9b9f652304678be031b55dfc0b0621941450d5fb398299fea155d84be926349","size_bytes":9}"#,
                Some(9),
            ),
            (
                "version_code overflows an android int",
                r#"{"version_name":"1.2.3","version_code":99999999999,"sha256":"d9b9f652304678be031b55dfc0b0621941450d5fb398299fea155d84be926349","size_bytes":9}"#,
                Some(9),
            ),
            (
                "zero version_code",
                r#"{"version_name":"1.2.3","version_code":0,"sha256":"d9b9f652304678be031b55dfc0b0621941450d5fb398299fea155d84be926349","size_bytes":9}"#,
                Some(9),
            ),
            (
                "negative version_code",
                r#"{"version_name":"1.2.3","version_code":-3,"sha256":"d9b9f652304678be031b55dfc0b0621941450d5fb398299fea155d84be926349","size_bytes":9}"#,
                Some(9),
            ),
            (
                "blank version_name",
                r#"{"version_name":"   ","version_code":12,"sha256":"d9b9f652304678be031b55dfc0b0621941450d5fb398299fea155d84be926349","size_bytes":9}"#,
                Some(9),
            ),
            (
                "sha256 is not a digest",
                r#"{"version_name":"1.2.3","version_code":12,"sha256":"unknown","size_bytes":9}"#,
                Some(9),
            ),
            (
                "sha256 is uppercase",
                r#"{"version_name":"1.2.3","version_code":12,"sha256":"D9B9F652304678BE031B55DFC0B0621941450D5FB398299FEA155D84BE926349","size_bytes":9}"#,
                Some(9),
            ),
            (
                "size_bytes is zero",
                r#"{"version_name":"1.2.3","version_code":12,"sha256":"d9b9f652304678be031b55dfc0b0621941450d5fb398299fea155d84be926349","size_bytes":0}"#,
                Some(9),
            ),
            // The SPA-fallback footgun: metadata with no APK behind it would
            // point the client at `index.html` served as 200 text/html.
            ("no apk behind the metadata", REAL_SIDECAR, None),
            ("apk size disagrees with the sidecar", REAL_SIDECAR, Some(11)),
        ] {
            let (_tmp, dir) = static_dir(Some(sidecar), apk_bytes);
            assert_eq!(
                load_bundled_release(&dir),
                ClientVersionResponse::none(),
                "{label} must degrade to the no-release payload"
            );
        }
    }

    #[test]
    fn a_directory_where_the_sidecar_should_be_is_survivable() {
        // An operator bind-mounting the wrong thing produces `IsADirectory`,
        // not `NotFound` — the branch that is easy to forget.
        let tmp = tempfile::tempdir().expect("temp dir");
        std::fs::create_dir(tmp.path().join(APK_METADATA_FILE_NAME)).expect("mkdir");
        assert_eq!(
            load_bundled_release(tmp.path()),
            ClientVersionResponse::none()
        );
    }

    #[test]
    fn only_a_lowercase_64_char_digest_is_a_sha256() {
        assert!(is_sha256_hex(&"a".repeat(64)));
        assert!(is_sha256_hex(&"0".repeat(64)));
        assert!(!is_sha256_hex(&"A".repeat(64)));
        assert!(!is_sha256_hex(&"g".repeat(64)));
        assert!(!is_sha256_hex(&"a".repeat(63)));
        assert!(!is_sha256_hex(&"a".repeat(65)));
        assert!(!is_sha256_hex(""));
    }
}
