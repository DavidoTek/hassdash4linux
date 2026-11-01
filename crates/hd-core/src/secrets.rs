//! Secrets in the system keyring (Secret Service, or the Flatpak secret
//! portal's file backend), via `oo7`.
//!
//! Every secret is stored with the attributes `application`, `server` and
//! `kind`. Because the server URL is part of the lookup, editing the URL in
//! the configuration file cannot make hassdash4linux send an existing token
//! to a different host: the lookup simply finds nothing.

use zeroize::Zeroizing;

use crate::APP_ID;

/// The kinds of secrets hassdash4linux stores.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SecretKind {
    /// OAuth refresh token.
    RefreshToken,
    /// Long-lived access token pasted by the user.
    LongLivedToken,
    /// mobile_app webhook ID (a bearer credential).
    WebhookId,
    /// mobile_app encryption secret.
    WebhookSecret,
}

impl SecretKind {
    pub const ALL: [SecretKind; 4] = [
        SecretKind::RefreshToken,
        SecretKind::LongLivedToken,
        SecretKind::WebhookId,
        SecretKind::WebhookSecret,
    ];

    fn as_str(self) -> &'static str {
        match self {
            SecretKind::RefreshToken => "refresh_token",
            SecretKind::LongLivedToken => "long_lived_token",
            SecretKind::WebhookId => "webhook_id",
            SecretKind::WebhookSecret => "webhook_secret",
        }
    }

    fn label(self) -> &'static str {
        match self {
            SecretKind::RefreshToken => "hassdash4linux: Home Assistant login",
            SecretKind::LongLivedToken => "hassdash4linux: Home Assistant access token",
            SecretKind::WebhookId => "hassdash4linux: companion webhook ID",
            SecretKind::WebhookSecret => "hassdash4linux: companion encryption key",
        }
    }
}

#[derive(Debug)]
pub struct SecretError(pub String);

impl std::fmt::Display for SecretError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "keyring error: {}", self.0)
    }
}

impl std::error::Error for SecretError {}

impl From<oo7::Error> for SecretError {
    fn from(e: oo7::Error) -> Self {
        SecretError(e.to_string())
    }
}

/// Handle to the keyring.
pub struct SecretStore {
    keyring: oo7::Keyring,
}

fn attrs(server: &str, kind: SecretKind) -> Vec<(&'static str, &str)> {
    vec![
        ("application", APP_ID),
        ("server", server),
        ("kind", kind.as_str()),
    ]
}

impl SecretStore {
    /// Open (and if needed unlock) the default keyring.
    pub async fn open() -> Result<Self, SecretError> {
        let keyring = oo7::Keyring::new().await?;
        // Unlocking may show a system prompt; a failure is reported on access.
        let _ = keyring.unlock().await;
        Ok(Self { keyring })
    }

    /// Read a secret.
    pub async fn get(
        &self,
        server: &str,
        kind: SecretKind,
    ) -> Result<Option<Zeroizing<String>>, SecretError> {
        let items = self.keyring.search_items(&attrs(server, kind)).await?;
        let Some(item) = items.into_iter().next() else {
            return Ok(None);
        };
        if item.is_locked().await? {
            item.unlock().await?;
        }
        let secret = item.secret().await?;
        let s =
            std::str::from_utf8(&secret).map_err(|_| SecretError("secret is not UTF-8".into()))?;
        Ok(Some(Zeroizing::new(s.to_owned())))
    }

    /// Store (or replace) a secret.
    pub async fn set(
        &self,
        server: &str,
        kind: SecretKind,
        value: &str,
    ) -> Result<(), SecretError> {
        self.keyring
            .create_item(kind.label(), &attrs(server, kind), value, true)
            .await?;
        Ok(())
    }

    /// Delete one secret (no error if it does not exist).
    pub async fn delete(&self, server: &str, kind: SecretKind) -> Result<(), SecretError> {
        self.keyring.delete(&attrs(server, kind)).await?;
        Ok(())
    }

    /// Delete every secret stored for `server`.
    pub async fn delete_all(&self, server: &str) -> Result<(), SecretError> {
        for kind in SecretKind::ALL {
            self.delete(server, kind).await?;
        }
        Ok(())
    }
}
