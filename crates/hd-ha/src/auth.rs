//! Authentication against Home Assistant.
//!
//! The browser login uses the OAuth2 authorization code flow with PKCE (S256)
//! and a loopback redirect (RFC 8252): we listen on `127.0.0.1:<random port>`,
//! the client ID is `http://127.0.0.1:<port>/` and the redirect URI
//! `http://127.0.0.1:<port>/callback`. Home Assistant accepts loopback client
//! IDs and redirect URIs on the same origin as the client ID.

use std::time::Duration;

use base64::Engine;
use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use url::Url;
use zeroize::Zeroizing;

use crate::HaError;
use crate::http::{Body, Http, error_message};
use hd_core::server::join;

/// How long we wait for the user to finish the browser login.
pub const LOGIN_TIMEOUT: Duration = Duration::from_secs(300);

/// Tokens returned by `/auth/token`.
pub struct TokenResponse {
    pub access_token: Zeroizing<String>,
    pub refresh_token: Option<Zeroizing<String>>,
    pub expires_in: u64,
}

impl std::fmt::Debug for TokenResponse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TokenResponse")
            .field("expires_in", &self.expires_in)
            .finish_non_exhaustive()
    }
}

/// Result of a completed browser login.
#[derive(Debug)]
pub struct OAuthLogin {
    pub client_id: String,
    pub tokens: TokenResponse,
}

/// Check that `base` is reachable, the certificate is trusted and the server
/// is Home Assistant (unauthenticated `GET /auth/providers`).
pub async fn check_server(http: &Http, base: &Url) -> Result<(), HaError> {
    let r = http.get(&join(base, "/auth/providers"), None).await?;
    if r.status == 400 && r.json.get("code").and_then(Value::as_str) == Some("onboarding_required")
    {
        return Err(HaError::Local(
            hd_core::i18n::tr(
                "Home Assistant is not set up yet. Finish the onboarding in your browser first.",
            )
            .into(),
        ));
    }
    if r.status != 200 {
        return Err(HaError::Protocol(format!(
            "/auth/providers returned HTTP {}",
            r.status
        )));
    }
    let looks_ok = r.json.is_array() || r.json.get("providers").is_some_and(Value::is_array);
    if !looks_ok {
        return Err(HaError::Protocol(
            "unexpected answer from /auth/providers".into(),
        ));
    }
    Ok(())
}

fn b64url(bytes: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

/// Constant-time string comparison.
fn ct_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

const SUCCESS_PAGE: &str = "<!doctype html><html><head><meta charset=\"utf-8\"><title>hassdash4linux</title>\
<style>body{font-family:system-ui,sans-serif;display:grid;place-items:center;height:90vh;margin:0}</style></head>\
<body><div><h1>&#10003; hassdash4linux</h1><p>__MSG__</p></div></body></html>";

/// Run the browser login. `open_browser` is called with the authorize URL.
pub async fn oauth_login(
    http: &Http,
    base: &Url,
    open_browser: impl FnOnce(Url),
) -> Result<OAuthLogin, HaError> {
    let listener = TcpListener::bind(("127.0.0.1", 0))
        .await
        .map_err(|e| HaError::Local(format!("cannot open the login listener: {e}")))?;
    let port = listener
        .local_addr()
        .map_err(|e| HaError::Local(e.to_string()))?
        .port();
    let client_id = format!("http://127.0.0.1:{port}/");
    let redirect_uri = format!("http://127.0.0.1:{port}/callback");
    let state = hd_core::rand::token(32);
    let verifier = Zeroizing::new(hd_core::rand::token(48));
    let challenge =
        b64url(ring::digest::digest(&ring::digest::SHA256, verifier.as_bytes()).as_ref());

    let mut authorize = join(base, "/auth/authorize");
    authorize
        .query_pairs_mut()
        .append_pair("response_type", "code")
        .append_pair("client_id", &client_id)
        .append_pair("redirect_uri", &redirect_uri)
        .append_pair("state", &state)
        .append_pair("code_challenge", &challenge)
        .append_pair("code_challenge_method", "S256");
    open_browser(authorize);

    let code = tokio::time::timeout(LOGIN_TIMEOUT, wait_for_code(&listener, &state))
        .await
        .map_err(|_| HaError::Cancelled)??;
    drop(listener);

    let r = http
        .post(
            &join(base, "/auth/token"),
            None,
            Body::Form(&[
                ("grant_type", "authorization_code"),
                ("code", &code),
                ("client_id", &client_id),
                ("code_verifier", &verifier),
            ]),
        )
        .await?;
    let tokens = parse_token_response(&r)?;
    if tokens.refresh_token.is_none() {
        return Err(HaError::Protocol("no refresh token received".into()));
    }
    Ok(OAuthLogin { client_id, tokens })
}

/// Accept connections on the loopback listener until one valid callback
/// arrives. Invalid requests get an error page and are otherwise ignored.
async fn wait_for_code(listener: &TcpListener, state: &str) -> Result<Zeroizing<String>, HaError> {
    loop {
        let (mut sock, _) = listener
            .accept()
            .await
            .map_err(|e| HaError::Local(e.to_string()))?;
        let mut buf = vec![0u8; 8192];
        let mut n = 0;
        // Read the request head (bounded in size and time).
        let read = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let m = sock.read(&mut buf[n..]).await?;
                if m == 0 {
                    break;
                }
                n += m;
                if buf[..n].windows(4).any(|w| w == b"\r\n\r\n") || n == buf.len() {
                    break;
                }
            }
            Ok::<_, std::io::Error>(())
        })
        .await;
        if !matches!(read, Ok(Ok(()))) {
            continue;
        }
        let head = String::from_utf8_lossy(&buf[..n]);
        let target = head.lines().next().and_then(|l| {
            let mut parts = l.split(' ');
            (parts.next() == Some("GET"))
                .then(|| parts.next())
                .flatten()
        });
        let parsed = target.and_then(|t| Url::parse(&format!("http://127.0.0.1{t}")).ok());
        let Some(url) = parsed.filter(|u| u.path() == "/callback") else {
            let _ = respond(&mut sock, "404 Not Found", "Not found.").await;
            continue;
        };
        let mut got_state = None;
        let mut code = None;
        let mut error = None;
        for (k, v) in url.query_pairs() {
            match k.as_ref() {
                "state" => got_state = Some(v.into_owned()),
                "code" => code = Some(Zeroizing::new(v.into_owned())),
                "error" => error = Some(v.into_owned()),
                _ => {}
            }
        }
        if !got_state.as_deref().is_some_and(|s| ct_eq(s, state)) {
            let _ = respond(&mut sock, "400 Bad Request", "Invalid login request.").await;
            continue;
        }
        if let Some(e) = error {
            let _ = respond(
                &mut sock,
                "200 OK",
                hd_core::i18n::tr("The login was cancelled. You can close this tab."),
            )
            .await;
            return Err(HaError::Auth(hd_core::truncate_chars(&e, 100)));
        }
        let Some(code) = code.filter(|c| !c.is_empty() && c.len() < 1024) else {
            let _ = respond(&mut sock, "400 Bad Request", "Missing code.").await;
            continue;
        };
        let _ = respond(
            &mut sock,
            "200 OK",
            hd_core::i18n::tr(
                "Login successful. You can close this tab and return to hassdash4linux.",
            ),
        )
        .await;
        return Ok(code);
    }
}

async fn respond(sock: &mut tokio::net::TcpStream, status: &str, msg: &str) -> std::io::Result<()> {
    let escaped = msg
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;");
    let body = SUCCESS_PAGE.replace("__MSG__", &escaped);
    let resp = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\n\
         Cache-Control: no-store\r\nReferrer-Policy: no-referrer\r\nX-Content-Type-Options: nosniff\r\n\
         Content-Security-Policy: default-src 'none'; style-src 'unsafe-inline'\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    sock.write_all(resp.as_bytes()).await?;
    sock.shutdown().await
}

fn parse_token_response(r: &crate::http::Response) -> Result<TokenResponse, HaError> {
    match r.status {
        200 => {}
        400 | 401 | 403 => return Err(HaError::Auth(error_message(r))),
        s => {
            return Err(HaError::Http {
                status: s,
                message: error_message(r),
            });
        }
    }
    let access = r
        .json
        .get("access_token")
        .and_then(Value::as_str)
        .ok_or_else(|| HaError::Protocol("no access token".into()))?;
    Ok(TokenResponse {
        access_token: Zeroizing::new(access.to_owned()),
        refresh_token: r
            .json
            .get("refresh_token")
            .and_then(Value::as_str)
            .map(|s| Zeroizing::new(s.to_owned())),
        expires_in: r
            .json
            .get("expires_in")
            .and_then(Value::as_u64)
            .unwrap_or(1800),
    })
}

/// Get a fresh access token from a refresh token.
pub async fn refresh(
    http: &Http,
    base: &Url,
    client_id: &str,
    refresh_token: &str,
) -> Result<TokenResponse, HaError> {
    let r = http
        .post(
            &join(base, "/auth/token"),
            None,
            Body::Form(&[
                ("grant_type", "refresh_token"),
                ("client_id", client_id),
                ("refresh_token", refresh_token),
            ]),
        )
        .await?;
    parse_token_response(&r)
}

/// Revoke a refresh token (and all access tokens derived from it). Best effort.
pub async fn revoke(http: &Http, base: &Url, refresh_token: &str) -> Result<(), HaError> {
    let r = http
        .post(
            &join(base, "/auth/revoke"),
            None,
            Body::Form(&[("token", refresh_token)]),
        )
        .await?;
    if r.status == 404 || r.status == 405 {
        // Older Home Assistant versions.
        http.post(
            &join(base, "/auth/token"),
            None,
            Body::Form(&[("token", refresh_token), ("action", "revoke")]),
        )
        .await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constant_time_eq() {
        assert!(ct_eq("abc", "abc"));
        assert!(!ct_eq("abc", "abd"));
        assert!(!ct_eq("abc", "ab"));
    }

    #[test]
    fn pkce_challenge_rfc7636() {
        // Test vector from RFC 7636, appendix B.
        let v = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        let c = b64url(ring::digest::digest(&ring::digest::SHA256, v.as_bytes()).as_ref());
        assert_eq!(c, "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM");
    }

    #[tokio::test]
    async fn loopback_callback() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let client = tokio::spawn(async move {
            async fn get(port: u16, path: &str) -> String {
                let mut s = tokio::net::TcpStream::connect(("127.0.0.1", port))
                    .await
                    .unwrap();
                s.write_all(format!("GET {path} HTTP/1.1\r\nHost: x\r\n\r\n").as_bytes())
                    .await
                    .unwrap();
                let mut out = String::new();
                s.read_to_string(&mut out).await.unwrap();
                out
            }
            assert!(get(port, "/favicon.ico").await.starts_with("HTTP/1.1 404"));
            assert!(
                get(port, "/callback?state=wrong&code=evil")
                    .await
                    .starts_with("HTTP/1.1 400")
            );
            assert!(
                get(port, "/callback?state=s3cret&code=good")
                    .await
                    .starts_with("HTTP/1.1 200")
            );
        });
        let code = wait_for_code(&listener, "s3cret").await.unwrap();
        assert_eq!(code.as_str(), "good");
        client.await.unwrap();
    }
}
