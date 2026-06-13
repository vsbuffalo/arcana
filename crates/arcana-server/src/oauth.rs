//! OAuth bridge for the MCP HTTP transport.
//!
//! **Scope (deliberate): single user, single client, loopback-first.** Arcana is
//! a local-first personal vault server (design goal: the vault never leaves the
//! machine). This endpoint exists only so MCP clients that expect an OAuth dance
//! (e.g. Claude) can authenticate *you* — there is one password and one
//! client credential, both from the launch environment.
//!
//! It is **not** a multi-tenant authorization server. The `/register` endpoint
//! is a deliberate stub: it always returns the single pre-configured `client_id`
//! so a client can discover it, and does not implement RFC 7591 dynamic client
//! registration despite the discovery metadata listing a `registration_endpoint`
//! (clients require the field to be present). Multi-user / multi-client access
//! over the network is out of scope — it would require per-user identity in the
//! vault core, not just here, and contradicts the local-first design. The real
//! protections are the password gate, PKCE (enforced), exact client-credential
//! match, and binding to loopback by default.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::extract::{Query, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use tokio::sync::RwLock;
use tracing::warn;

const AUTH_CODE_TTL: Duration = Duration::from_secs(120);
const ACCESS_TOKEN_TTL: Duration = Duration::from_secs(86400);

// ---------------------------------------------------------------------------
// Config & state
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct OAuthConfig {
    pub client_id: String,
    pub client_secret: String,
    pub password: String,
}

struct StoredAuthCode {
    /// The redirect_uri bound at authorize time; re-checked at the token endpoint.
    redirect_uri: String,
    code_challenge: String,
    created_at: Instant,
}

struct StoredToken {
    created_at: Instant,
}

#[derive(Clone)]
pub struct OAuthState {
    config: OAuthConfig,
    /// Optional static bearer token accepted alongside OAuth tokens.
    static_bearer: Option<String>,
    auth_codes: Arc<RwLock<HashMap<String, StoredAuthCode>>>,
    access_tokens: Arc<RwLock<HashMap<String, StoredToken>>>,
}

impl OAuthState {
    pub fn new(config: OAuthConfig, static_bearer: Option<String>) -> Self {
        Self {
            config,
            static_bearer,
            auth_codes: Arc::new(RwLock::new(HashMap::new())),
            access_tokens: Arc::new(RwLock::new(HashMap::new())),
        }
    }
}

// ---------------------------------------------------------------------------
// Discovery endpoints
// ---------------------------------------------------------------------------

#[derive(serde::Serialize)]
struct AuthServerMetadata {
    issuer: String,
    authorization_endpoint: String,
    token_endpoint: String,
    registration_endpoint: String,
    response_types_supported: Vec<String>,
    grant_types_supported: Vec<String>,
    code_challenge_methods_supported: Vec<String>,
    token_endpoint_auth_methods_supported: Vec<String>,
}

pub async fn metadata(headers: axum::http::HeaderMap) -> impl IntoResponse {
    let host = extract_host(&headers);
    let base = base_url(&host);
    axum::Json(AuthServerMetadata {
        issuer: base.clone(),
        authorization_endpoint: format!("{base}/authorize"),
        token_endpoint: format!("{base}/token"),
        registration_endpoint: format!("{base}/register"),
        response_types_supported: vec!["code".into()],
        grant_types_supported: vec!["authorization_code".into()],
        code_challenge_methods_supported: vec!["S256".into()],
        token_endpoint_auth_methods_supported: vec![
            "client_secret_post".into(),
            "client_secret_basic".into(),
        ],
    })
}

#[derive(serde::Serialize)]
struct ProtectedResourceMetadata {
    resource: String,
    authorization_servers: Vec<String>,
}

pub async fn protected_resource(headers: axum::http::HeaderMap) -> impl IntoResponse {
    let host = extract_host(&headers);
    let base = base_url(&host);
    axum::Json(ProtectedResourceMetadata {
        resource: base.clone(),
        authorization_servers: vec![base],
    })
}

// ---------------------------------------------------------------------------
// Dynamic client registration (RFC 7591)
// ---------------------------------------------------------------------------

#[derive(serde::Deserialize)]
pub struct RegisterRequest {
    #[allow(dead_code)]
    client_name: Option<String>,
    #[allow(dead_code)]
    redirect_uris: Option<Vec<String>>,
}

#[derive(serde::Serialize)]
struct RegisterResponse {
    client_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    client_secret: Option<String>,
    client_id_issued_at: u64,
    client_secret_expires_at: u64,
    redirect_uris: Vec<String>,
}

pub async fn register(
    State(state): State<OAuthState>,
    axum::Json(body): axum::Json<RegisterRequest>,
) -> impl IntoResponse {
    axum::Json(RegisterResponse {
        client_id: state.config.client_id.clone(),
        client_secret: None,
        client_id_issued_at: 0,
        client_secret_expires_at: 0,
        redirect_uris: body.redirect_uris.unwrap_or_default(),
    })
}

// ---------------------------------------------------------------------------
// Authorization endpoint
// ---------------------------------------------------------------------------

#[derive(serde::Deserialize)]
pub struct AuthorizeParams {
    #[allow(dead_code)]
    response_type: Option<String>,
    #[allow(dead_code)]
    client_id: Option<String>,
    redirect_uri: String,
    code_challenge: String,
    #[allow(dead_code)]
    code_challenge_method: Option<String>,
    state: Option<String>,
    #[allow(dead_code)]
    scope: Option<String>,
}

pub async fn authorize_form(Query(params): Query<AuthorizeParams>) -> impl IntoResponse {
    axum::response::Html(format!(
        r#"<!DOCTYPE html>
<html><head><title>Arcana MCP</title>
<meta name="viewport" content="width=device-width, initial-scale=1">
<style>
body {{ font-family: system-ui, sans-serif; max-width: 380px; margin: 80px auto; padding: 20px; }}
h2 {{ margin-bottom: 4px; }}
p {{ color: #666; margin-top: 4px; }}
input {{ display: block; width: 100%; padding: 10px; margin: 10px 0; box-sizing: border-box;
         border: 1px solid #ccc; border-radius: 4px; font-size: 16px; }}
button {{ padding: 12px; width: 100%; background: #111; color: #fff; border: none;
          border-radius: 4px; font-size: 16px; cursor: pointer; }}
button:hover {{ background: #333; }}
</style></head><body>
<h2>Arcana MCP</h2>
<p>Authorize access to your vault.</p>
<form method="POST" action="/authorize">
<input type="hidden" name="redirect_uri" value="{}" />
<input type="hidden" name="code_challenge" value="{}" />
<input type="hidden" name="state" value="{}" />
<input type="password" name="password" placeholder="Password" autofocus />
<button type="submit">Authorize</button>
</form></body></html>"#,
        html_escape(&params.redirect_uri),
        html_escape(&params.code_challenge),
        html_escape(params.state.as_deref().unwrap_or("")),
    ))
}

#[derive(serde::Deserialize)]
pub struct AuthorizeSubmit {
    redirect_uri: String,
    code_challenge: String,
    state: Option<String>,
    password: String,
}

pub async fn authorize_submit(
    State(state): State<OAuthState>,
    axum::Form(form): axum::Form<AuthorizeSubmit>,
) -> Response {
    if !ct_eq(&form.password, &state.config.password) {
        return (StatusCode::UNAUTHORIZED, "Invalid password").into_response();
    }

    // Validate redirect_uri: must be HTTPS or localhost
    if !is_valid_redirect_uri(&form.redirect_uri) {
        return (StatusCode::BAD_REQUEST, "Invalid redirect_uri").into_response();
    }

    let code = uuid::Uuid::new_v4().to_string();

    {
        let mut codes = state.auth_codes.write().await;
        codes.insert(
            code.clone(),
            StoredAuthCode {
                redirect_uri: form.redirect_uri.clone(),
                code_challenge: form.code_challenge,
                created_at: Instant::now(),
            },
        );
    }

    let sep = if form.redirect_uri.contains('?') {
        "&"
    } else {
        "?"
    };
    let mut url = format!("{}{}code={}", form.redirect_uri, sep, code);
    if let Some(st) = &form.state {
        if !st.is_empty() {
            url.push_str(&format!("&state={st}"));
        }
    }

    Redirect::to(&url).into_response()
}

// ---------------------------------------------------------------------------
// Token endpoint
// ---------------------------------------------------------------------------

#[derive(serde::Deserialize)]
pub struct TokenRequest {
    grant_type: String,
    code: Option<String>,
    code_verifier: Option<String>,
    #[serde(default)]
    client_id: Option<String>,
    #[serde(default)]
    client_secret: Option<String>,
    #[serde(default)]
    redirect_uri: Option<String>,
}

#[derive(serde::Serialize)]
struct TokenResponse {
    access_token: String,
    token_type: String,
    expires_in: u64,
}

pub async fn token(
    State(state): State<OAuthState>,
    headers: axum::http::HeaderMap,
    axum::Form(form): axum::Form<TokenRequest>,
) -> Response {
    if form.grant_type != "authorization_code" {
        return error_json(StatusCode::BAD_REQUEST, "unsupported_grant_type");
    }

    // Extract client credentials from body or Basic auth header
    let (client_id, client_secret) =
        match extract_client_credentials(&headers, &form.client_id, &form.client_secret) {
            Some(creds) => creds,
            None => return error_json(StatusCode::UNAUTHORIZED, "missing client credentials"),
        };

    // Evaluate both before combining so the check doesn't short-circuit on the
    // client_id and leak (via timing) whether the secret was even compared.
    let id_ok = ct_eq(&client_id, &state.config.client_id);
    let secret_ok = ct_eq(&client_secret, &state.config.client_secret);
    if !(id_ok && secret_ok) {
        return error_json(StatusCode::UNAUTHORIZED, "invalid_client");
    }

    let code = match &form.code {
        Some(c) => c,
        None => return error_json(StatusCode::BAD_REQUEST, "missing code"),
    };

    let code_verifier = match &form.code_verifier {
        Some(v) => v,
        None => return error_json(StatusCode::BAD_REQUEST, "missing code_verifier"),
    };

    // Look up and consume auth code
    let stored = {
        let mut codes = state.auth_codes.write().await;
        codes.remove(code)
    };

    let stored = match stored {
        Some(s) => s,
        None => return error_json(StatusCode::BAD_REQUEST, "invalid_grant"),
    };

    if stored.created_at.elapsed() > AUTH_CODE_TTL {
        return error_json(StatusCode::BAD_REQUEST, "invalid_grant");
    }

    // Defense-in-depth (PKCE below is the primary binding): if the client sends a
    // redirect_uri, it must match the one bound at authorize time (RFC 6749
    // §4.1.3). We don't *require* its presence so a client that omits it still
    // works, but a mismatched value is rejected.
    if let Some(req_redirect) = &form.redirect_uri {
        if req_redirect != &stored.redirect_uri {
            warn!("token request redirect_uri does not match authorize-time value");
            return error_json(StatusCode::BAD_REQUEST, "invalid_grant");
        }
    }

    // PKCE verification: BASE64URL(SHA256(code_verifier)) must equal code_challenge
    let computed = URL_SAFE_NO_PAD.encode(Sha256::digest(code_verifier.as_bytes()));
    if computed != stored.code_challenge {
        warn!("PKCE verification failed");
        return error_json(StatusCode::BAD_REQUEST, "invalid_grant");
    }

    // Issue access token
    let access_token = uuid::Uuid::new_v4().to_string();
    {
        let mut tokens = state.access_tokens.write().await;
        tokens.insert(
            access_token.clone(),
            StoredToken {
                created_at: Instant::now(),
            },
        );
    }

    axum::Json(TokenResponse {
        access_token,
        token_type: "bearer".into(),
        expires_in: ACCESS_TOKEN_TTL.as_secs(),
    })
    .into_response()
}

// ---------------------------------------------------------------------------
// Auth middleware
// ---------------------------------------------------------------------------

pub async fn bearer_auth(
    State(state): State<OAuthState>,
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    let token = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "));

    let token = match token {
        Some(t) => t,
        None => return unauthorized(),
    };

    // Accept static bearer token if configured
    if let Some(ref static_token) = state.static_bearer {
        if ct_eq(token, static_token) {
            return next.run(req).await;
        }
    }

    // Check OAuth-issued tokens and prune expired ones opportunistically
    let valid = {
        let mut tokens = state.access_tokens.write().await;
        let is_valid =
            matches!(tokens.get(token), Some(t) if t.created_at.elapsed() < ACCESS_TOKEN_TTL);
        tokens.retain(|_, t| t.created_at.elapsed() < ACCESS_TOKEN_TTL);
        is_valid
    };

    if valid {
        next.run(req).await
    } else {
        unauthorized()
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn unauthorized() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        [(header::WWW_AUTHENTICATE, "Bearer")],
        "Unauthorized",
    )
        .into_response()
}

fn error_json(status: StatusCode, error: &str) -> Response {
    (status, axum::Json(serde_json::json!({ "error": error }))).into_response()
}

fn extract_client_credentials(
    headers: &axum::http::HeaderMap,
    form_id: &Option<String>,
    form_secret: &Option<String>,
) -> Option<(String, String)> {
    // Try form body (client_secret_post)
    if let (Some(id), Some(secret)) = (form_id, form_secret) {
        return Some((id.clone(), secret.clone()));
    }

    // Try Basic auth (client_secret_basic)
    let auth = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())?;
    let encoded = auth.strip_prefix("Basic ")?;
    let decoded = String::from_utf8(
        base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .ok()?,
    )
    .ok()?;
    let (id, secret) = decoded.split_once(':')?;
    Some((id.to_string(), secret.to_string()))
}

/// Validate an OAuth redirect URI: HTTPS anywhere, or HTTP only to loopback.
///
/// The loopback check requires a delimiter (`:`, `/`, or end-of-string) right
/// after `localhost`/`127.0.0.1` so an attacker host that merely *starts with*
/// the loopback name — `http://localhost.evil.com` — is rejected. (A bare prefix
/// check would accept it.)
fn is_valid_redirect_uri(uri: &str) -> bool {
    if uri.starts_with("https://") {
        return true;
    }
    for host in ["http://localhost", "http://127.0.0.1"] {
        if let Some(rest) = uri.strip_prefix(host) {
            // Next char must end the authority, not extend the hostname.
            match rest.chars().next() {
                None | Some(':') | Some('/') => return true,
                _ => {}
            }
        }
    }
    false
}

fn extract_host(headers: &axum::http::HeaderMap) -> String {
    headers
        .get(axum::http::header::HOST)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("localhost")
        .to_string()
}

fn base_url(host: &str) -> String {
    if host.starts_with("localhost") || host.starts_with("127.0.0.1") {
        format!("http://{host}")
    } else {
        format!("https://{host}")
    }
}

/// Compare two secrets in constant time over their content, so an attacker
/// can't recover a secret byte-by-byte from response-timing differences.
/// (Length may still differ observably — that's the standard tradeoff.)
fn ct_eq(a: &str, b: &str) -> bool {
    a.as_bytes().ct_eq(b.as_bytes()).into()
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#x27;")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ct_eq_behaves_like_equality() {
        assert!(ct_eq("hunter2", "hunter2"));
        assert!(!ct_eq("hunter2", "hunter3"));
        assert!(!ct_eq("short", "a-longer-secret"));
        assert!(ct_eq("", ""));
    }

    #[test]
    fn redirect_uri_accepts_https_and_loopback() {
        assert!(is_valid_redirect_uri("https://claude.ai/callback"));
        assert!(is_valid_redirect_uri("http://localhost"));
        assert!(is_valid_redirect_uri("http://localhost/cb"));
        assert!(is_valid_redirect_uri("http://localhost:8080/cb"));
        assert!(is_valid_redirect_uri("http://127.0.0.1:1234/cb"));
    }

    #[test]
    fn redirect_uri_rejects_loopback_prefix_bypass() {
        // Hosts that merely *start with* the loopback name must be rejected —
        // the bug a bare prefix check would let through.
        assert!(!is_valid_redirect_uri("http://localhost.evil.com/cb"));
        assert!(!is_valid_redirect_uri("http://localhostevil/cb"));
        assert!(!is_valid_redirect_uri("http://127.0.0.1.evil.com/cb"));
        // Plain HTTP to a non-loopback host, and non-http schemes.
        assert!(!is_valid_redirect_uri("http://evil.com/cb"));
        assert!(!is_valid_redirect_uri("ftp://localhost/cb"));
    }
}
