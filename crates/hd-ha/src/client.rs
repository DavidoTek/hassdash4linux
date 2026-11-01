//! Credentials, token handling and connection setup shared by the daemon and the UI.

use std::time::{Duration, Instant};

use tokio::sync::Mutex;
use url::Url;
use zeroize::Zeroizing;

use hd_core::config::{AuthKind, Config};
use hd_core::secrets::{SecretKind, SecretStore};

use crate::http::Http;
use crate::mobile_app::Registration;
use crate::tls::Tls;
use crate::{HaError, Session};

/// How hassdash4linux authenticates.
pub enum Credentials {
    LongLived(Zeroizing<String>),
    OAuth {
        client_id: String,
        refresh_token: Zeroizing<String>,
    },
}

/// Everything needed to talk to one Home Assistant instance.
pub struct Connector {
    pub base: Url,
    pub tls: Tls,
    pub http: Http,
    creds: Credentials,
    cache: Mutex<Option<(Zeroizing<String>, Instant)>>,
}

impl Connector {
    pub fn new(base: Url, pin: Option<&str>, creds: Credentials) -> Result<Self, HaError> {
        let tls = crate::tls::build(pin)?;
        let http = Http::new(&tls)?;
        Ok(Self {
            base,
            tls,
            http,
            creds,
            cache: Mutex::new(None),
        })
    }

    /// Build from the configuration and the keyring.
    pub async fn from_config(cfg: &Config, store: &SecretStore) -> Result<Self, HaError> {
        let conn = cfg
            .connection
            .as_ref()
            .ok_or_else(|| HaError::Local("not configured".into()))?;
        let base =
            hd_core::server::validate(&conn.url).map_err(|e| HaError::Local(e.to_string()))?;
        let missing = || {
            HaError::Auth(hd_core::i18n::tr("The login data is missing from the keyring.").into())
        };
        let creds = match conn.auth {
            AuthKind::LongLivedToken => Credentials::LongLived(
                store
                    .get(&conn.url, SecretKind::LongLivedToken)
                    .await?
                    .ok_or_else(missing)?,
            ),
            AuthKind::Oauth => Credentials::OAuth {
                client_id: conn.client_id.clone().ok_or_else(missing)?,
                refresh_token: store
                    .get(&conn.url, SecretKind::RefreshToken)
                    .await?
                    .ok_or_else(missing)?,
            },
        };
        Self::new(base, conn.tls_pin.as_deref(), creds)
    }

    pub fn credentials(&self) -> &Credentials {
        &self.creds
    }

    /// A valid access token (refreshed when needed for OAuth).
    pub async fn access_token(&self) -> Result<Zeroizing<String>, HaError> {
        match &self.creds {
            Credentials::LongLived(t) => Ok(t.clone()),
            Credentials::OAuth {
                client_id,
                refresh_token,
            } => {
                let mut cache = self.cache.lock().await;
                if let Some((tok, until)) = cache.as_ref()
                    && Instant::now() < *until
                {
                    return Ok(tok.clone());
                }
                let t =
                    crate::auth::refresh(&self.http, &self.base, client_id, refresh_token).await?;
                let lifetime = Duration::from_secs(t.expires_in.saturating_sub(60).max(30));
                *cache = Some((t.access_token.clone(), Instant::now() + lifetime));
                Ok(t.access_token)
            }
        }
    }

    /// Drop a cached access token (after it was rejected).
    pub async fn invalidate(&self) {
        *self.cache.lock().await = None;
    }

    /// Open an authenticated WebSocket session.
    pub async fn connect(&self) -> Result<Session, HaError> {
        let token = self.access_token().await?;
        match Session::connect(&self.base, &self.tls, &token).await {
            Err(HaError::Auth(_)) if matches!(self.creds, Credentials::OAuth { .. }) => {
                // The cached access token may have been revoked; refresh once.
                self.invalidate().await;
                let token = self.access_token().await?;
                Session::connect(&self.base, &self.tls, &token).await
            }
            other => other,
        }
    }
}

/// Load the companion registration from the keyring, if registered.
pub async fn load_registration(
    cfg: &Config,
    store: &SecretStore,
) -> Result<Option<Registration>, HaError> {
    let (Some(conn), Some(comp)) = (&cfg.connection, &cfg.companion) else {
        return Ok(None);
    };
    if !comp.registered {
        return Ok(None);
    }
    let Some(webhook_id) = store.get(&conn.url, SecretKind::WebhookId).await? else {
        return Ok(None);
    };
    let secret = store.get(&conn.url, SecretKind::WebhookSecret).await?;
    Ok(Some(Registration { webhook_id, secret }))
}

/// Store a registration in the keyring.
pub async fn save_registration(
    server: &str,
    store: &SecretStore,
    reg: &Registration,
) -> Result<(), HaError> {
    store
        .set(server, SecretKind::WebhookId, &reg.webhook_id)
        .await?;
    match &reg.secret {
        Some(s) => store.set(server, SecretKind::WebhookSecret, s).await?,
        None => store.delete(server, SecretKind::WebhookSecret).await?,
    }
    Ok(())
}
