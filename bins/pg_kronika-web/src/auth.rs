//! Optional HTTP Basic Auth for the protected routes.
//!
//! The expected `Authorization` header is computed once from the configured
//! credentials; each request is compared against it in constant time.

use axum::body::Body;
use axum::extract::State;
use axum::http::{Request, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use base64::Engine as _;
use subtle::ConstantTimeEq as _;

use crate::problem::ApiProblem;

/// The exact `Authorization` header value an authenticated request must send.
#[derive(Clone)]
pub struct AuthConfig {
    expected_header: String,
}

impl AuthConfig {
    /// Build the expected header from a username and password.
    #[must_use]
    pub fn new(user: &str, pass: &str) -> Self {
        let encoded = base64::engine::general_purpose::STANDARD.encode(format!("{user}:{pass}"));
        Self {
            expected_header: format!("Basic {encoded}"),
        }
    }
}

impl std::fmt::Debug for AuthConfig {
    /// Hide the credential so it never reaches a log or panic message.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuthConfig").finish_non_exhaustive()
    }
}

/// Whether `header` is exactly the expected Basic credential.
///
/// Constant-time in the header contents so a caller cannot learn how many
/// leading bytes matched by timing repeated guesses.
pub(crate) fn check_basic_auth(header: Option<&str>, cfg: &AuthConfig) -> bool {
    let Some(header) = header else {
        return false;
    };
    header
        .as_bytes()
        .ct_eq(cfg.expected_header.as_bytes())
        .into()
}

/// Reject requests that do not carry the expected credential with `401`.
pub(crate) async fn require_basic_auth(
    State(cfg): State<AuthConfig>,
    req: Request<Body>,
    next: Next,
) -> Response {
    let header = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok());
    if check_basic_auth(header, &cfg) {
        next.run(req).await
    } else {
        ApiProblem::unauthorized().into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::{AuthConfig, check_basic_auth};
    use base64::Engine as _;

    fn header_for(user: &str, pass: &str) -> String {
        let encoded = base64::engine::general_purpose::STANDARD.encode(format!("{user}:{pass}"));
        format!("Basic {encoded}")
    }

    #[test]
    fn correct_credentials_pass() {
        let cfg = AuthConfig::new("alice", "secret");
        assert!(
            check_basic_auth(Some(&header_for("alice", "secret")), &cfg),
            "the exact credential is accepted"
        );
    }

    #[test]
    fn wrong_password_fails() {
        let cfg = AuthConfig::new("alice", "secret");
        assert!(
            !check_basic_auth(Some(&header_for("alice", "wrong")), &cfg),
            "a wrong password is rejected"
        );
    }

    #[test]
    fn wrong_user_fails() {
        let cfg = AuthConfig::new("alice", "secret");
        assert!(
            !check_basic_auth(Some(&header_for("bob", "secret")), &cfg),
            "a wrong user is rejected"
        );
    }

    #[test]
    fn missing_header_fails() {
        let cfg = AuthConfig::new("alice", "secret");
        assert!(
            !check_basic_auth(None, &cfg),
            "no Authorization header is rejected"
        );
    }

    #[test]
    fn non_basic_scheme_fails() {
        let cfg = AuthConfig::new("alice", "secret");
        assert!(
            !check_basic_auth(Some("Bearer sometoken"), &cfg),
            "a non-Basic scheme is rejected"
        );
    }

    #[test]
    fn malformed_credential_fails() {
        let cfg = AuthConfig::new("alice", "secret");
        assert!(
            !check_basic_auth(Some("Basic !!!not-base64!!!"), &cfg),
            "a garbage credential is rejected"
        );
    }
}
