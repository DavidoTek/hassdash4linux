//! Minimal REST client on top of reqwest with our TLS configuration.
//!
//! Redirects are disabled (credentials must never follow a redirect to
//! another host) and response bodies are size-limited.

use std::time::Duration;

use serde_json::Value;
use url::Url;

use crate::HaError;
use crate::tls::Tls;

/// Maximum accepted response body size.
const MAX_BODY: usize = 8 * 1024 * 1024;

/// Request body.
pub enum Body<'a> {
    None,
    Json(&'a Value),
    Form(&'a [(&'a str, &'a str)]),
}

#[derive(Clone)]
pub struct Http {
    client: reqwest::Client,
    tls: Tls,
}

/// A response: status code and parsed JSON (or `Null` for empty/non-JSON bodies).
#[derive(Debug)]
pub struct Response {
    pub status: u16,
    pub json: Value,
}

impl Http {
    pub fn new(tls: &Tls) -> Result<Self, HaError> {
        let client = reqwest::Client::builder()
            .use_preconfigured_tls((*tls.config).clone())
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(30))
            .user_agent(crate::user_agent())
            .build()
            .map_err(|e| HaError::Local(format!("HTTP client setup failed: {e}")))?;
        Ok(Self {
            client,
            tls: tls.clone(),
        })
    }

    pub fn tls(&self) -> &Tls {
        &self.tls
    }

    /// Perform a request. Network and TLS failures become errors; any HTTP
    /// status is returned as-is.
    pub async fn request(
        &self,
        method: reqwest::Method,
        url: &Url,
        bearer: Option<&str>,
        body: Body<'_>,
    ) -> Result<Response, HaError> {
        let mut req = self
            .client
            .request(method, url.clone())
            .header("Accept", "application/json");
        if let Some(t) = bearer {
            req = req.bearer_auth(t);
        }
        match body {
            Body::None => {}
            Body::Json(v) => {
                req = req
                    .header("Content-Type", "application/json")
                    .body(serde_json::to_vec(v).map_err(|e| HaError::Local(e.to_string()))?);
            }
            Body::Form(pairs) => {
                let encoded = url::form_urlencoded::Serializer::new(String::new())
                    .extend_pairs(pairs)
                    .finish();
                req = req
                    .header("Content-Type", "application/x-www-form-urlencoded")
                    .body(encoded);
            }
        }
        self.tls.capture.take();
        let mut resp = req.send().await.map_err(|e| self.map_err(e))?;
        let status = resp.status().as_u16();
        let mut buf = Vec::new();
        while let Some(chunk) = resp.chunk().await.map_err(|e| self.map_err(e))? {
            if buf.len() + chunk.len() > MAX_BODY {
                return Err(HaError::Protocol("response too large".into()));
            }
            buf.extend_from_slice(&chunk);
        }
        let json = if buf.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&buf).unwrap_or(Value::Null)
        };
        Ok(Response { status, json })
    }

    pub async fn get(&self, url: &Url, bearer: Option<&str>) -> Result<Response, HaError> {
        self.request(reqwest::Method::GET, url, bearer, Body::None)
            .await
    }

    pub async fn post(
        &self,
        url: &Url,
        bearer: Option<&str>,
        body: Body<'_>,
    ) -> Result<Response, HaError> {
        self.request(reqwest::Method::POST, url, bearer, body).await
    }

    fn map_err(&self, e: reqwest::Error) -> HaError {
        if let Some(fp) = self.tls.capture.take() {
            return HaError::Tls {
                fingerprint: Some(fp),
                message: root_cause(&e),
            };
        }
        if e.is_timeout() {
            HaError::Timeout
        } else {
            HaError::Network(root_cause(&e))
        }
    }
}

/// The innermost error message (reqwest wraps the interesting part).
pub(crate) fn root_cause(e: &dyn std::error::Error) -> String {
    let mut cur: &dyn std::error::Error = e;
    while let Some(next) = cur.source() {
        cur = next;
    }
    cur.to_string()
}

/// Short message from an error response body.
pub(crate) fn error_message(r: &Response) -> String {
    r.json
        .get("message")
        .or_else(|| r.json.get("error_description"))
        .or_else(|| r.json.get("error"))
        .and_then(Value::as_str)
        .map(|s| hd_core::truncate_chars(s, 200))
        .unwrap_or_default()
}
