//! Companion device support through Home Assistant's `mobile_app` integration:
//! registration, encrypted webhooks and the local push notification channel.
//!
//! Webhook payloads are encrypted with libsodium's `crypto_secretbox`
//! (XSalsa20-Poly1305). The key is the hex-decoded registration secret
//! (Home Assistant's "non-legacy" mode); responses are also tried with the
//! legacy key (UTF-8 bytes of the secret, truncated/padded to 32 bytes).

use base64::Engine;
use crypto_secretbox::aead::{Aead, KeyInit};
use crypto_secretbox::{Key, Nonce, XSalsa20Poly1305};
use serde_json::{Value, json};
use url::Url;
use zeroize::Zeroizing;

use crate::HaError;
use crate::http::{Body, Http, error_message};
use hd_core::server::join;

/// Credentials returned by a registration.
#[derive(Clone)]
pub struct Registration {
    pub webhook_id: Zeroizing<String>,
    pub secret: Option<Zeroizing<String>>,
}

impl std::fmt::Debug for Registration {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Registration { .. }")
    }
}

/// Information about this device sent to Home Assistant.
#[derive(Debug, Clone)]
pub struct DeviceInfo {
    pub device_id: String,
    pub device_name: String,
    pub manufacturer: String,
    pub model: String,
    pub os_version: String,
}

fn read_trimmed(path: &str) -> Option<String> {
    let s = std::fs::read_to_string(path).ok()?;
    let s = s.trim();
    (!s.is_empty() && !s.chars().any(char::is_control)).then(|| hd_core::truncate_chars(s, 64))
}

fn os_release() -> Option<String> {
    // In Flatpak the host's file is exposed under /run/host.
    let text = [
        "/run/host/os-release",
        "/etc/os-release",
        "/usr/lib/os-release",
    ]
    .iter()
    .find_map(|p| std::fs::read_to_string(p).ok())?;
    let get = |key: &str| {
        text.lines()
            .find_map(|l| l.strip_prefix(key)?.strip_prefix('='))
            .map(|v| v.trim().trim_matches('"').to_owned())
            .filter(|v| !v.is_empty())
    };
    get("PRETTY_NAME")
        .or_else(|| get("NAME"))
        .map(|s| hd_core::truncate_chars(&s, 64))
}

impl DeviceInfo {
    /// Collect hardware and OS information. Uses the random `device_id`
    /// from the configuration, never `/etc/machine-id`.
    pub fn collect(device_id: &str, device_name: &str) -> Self {
        Self {
            device_id: device_id.to_owned(),
            device_name: device_name.to_owned(),
            manufacturer: read_trimmed("/sys/class/dmi/id/sys_vendor")
                .unwrap_or_else(|| "Linux".into()),
            model: read_trimmed("/sys/class/dmi/id/product_name").unwrap_or_else(|| "PC".into()),
            os_version: os_release().unwrap_or_else(|| "Linux".into()),
        }
    }
}

/// The default device name: the short host name (`laptop` for `laptop.fritz.box`).
pub fn default_device_name() -> String {
    read_trimmed("/proc/sys/kernel/hostname")
        .or_else(|| read_trimmed("/etc/hostname"))
        .and_then(|h| h.split('.').next().map(str::to_owned))
        .filter(|h| !h.is_empty())
        .unwrap_or_else(|| "linux".into())
}

/// Register this installation as a mobile_app device.
pub async fn register(
    http: &Http,
    base: &Url,
    access_token: &str,
    info: &DeviceInfo,
) -> Result<Registration, HaError> {
    let body = json!({
        "device_id": info.device_id,
        "app_id": hd_core::APP_ID,
        "app_name": hd_core::APP_NAME,
        "app_version": hd_core::VERSION,
        "device_name": info.device_name,
        "manufacturer": info.manufacturer,
        "model": info.model,
        "os_name": "Linux",
        "os_version": info.os_version,
        "supports_encryption": true,
        "app_data": {"push_websocket_channel": true},
    });
    let r = http
        .post(
            &join(base, "/api/mobile_app/registrations"),
            Some(access_token),
            Body::Json(&body),
        )
        .await?;
    match r.status {
        200 | 201 => {}
        404 => {
            return Err(HaError::Http {
                status: 404,
                message: hd_core::i18n::tr(
                    "The mobile_app integration is not enabled in Home Assistant.",
                )
                .into(),
            });
        }
        401 | 403 => return Err(HaError::Auth(error_message(&r))),
        s => {
            return Err(HaError::Http {
                status: s,
                message: error_message(&r),
            });
        }
    }
    let webhook_id = r
        .json
        .get("webhook_id")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty() && s.len() < 256)
        .ok_or_else(|| HaError::Protocol("no webhook_id in registration".into()))?;
    Ok(Registration {
        webhook_id: Zeroizing::new(webhook_id.to_owned()),
        secret: r
            .json
            .get("secret")
            .and_then(Value::as_str)
            .map(|s| Zeroizing::new(s.to_owned())),
    })
}

fn key_hex(secret: &str) -> Option<Zeroizing<[u8; 32]>> {
    if secret.len() != 64 {
        return None;
    }
    let mut key = Zeroizing::new([0u8; 32]);
    for (i, chunk) in secret.as_bytes().chunks(2).enumerate() {
        let s = std::str::from_utf8(chunk).ok()?;
        key[i] = u8::from_str_radix(s, 16).ok()?;
    }
    Some(key)
}

fn key_legacy(secret: &str) -> Zeroizing<[u8; 32]> {
    let mut key = Zeroizing::new([0u8; 32]);
    let b = secret.as_bytes();
    let n = b.len().min(32);
    key[..n].copy_from_slice(&b[..n]);
    key
}

/// Encrypt `plaintext` as Home Assistant expects: base64(nonce ‖ tag ‖ ciphertext).
pub fn encrypt(key: &[u8; 32], plaintext: &[u8]) -> String {
    let cipher = XSalsa20Poly1305::new(Key::from_slice(key));
    let mut nonce = [0u8; 24];
    hd_core::rand::fill(&mut nonce);
    let ct = cipher
        .encrypt(Nonce::from_slice(&nonce), plaintext)
        .expect("encryption cannot fail");
    let mut out = nonce.to_vec();
    out.extend_from_slice(&ct);
    base64::engine::general_purpose::STANDARD.encode(out)
}

/// Decrypt base64(nonce ‖ tag ‖ ciphertext).
pub fn decrypt(key: &[u8; 32], data: &str) -> Option<Vec<u8>> {
    let raw = base64::engine::general_purpose::STANDARD
        .decode(data.trim())
        .ok()?;
    if raw.len() < 24 + 16 {
        return None;
    }
    let (nonce, ct) = raw.split_at(24);
    XSalsa20Poly1305::new(Key::from_slice(key))
        .decrypt(Nonce::from_slice(nonce), ct)
        .ok()
}

/// Call the registration's webhook with `type` and `data`.
pub async fn webhook(
    http: &Http,
    base: &Url,
    reg: &Registration,
    kind: &str,
    data: Value,
) -> Result<Value, HaError> {
    let body = match reg.secret.as_deref().and_then(|s| key_hex(s)) {
        Some(key) => {
            let plain = Zeroizing::new(
                serde_json::to_vec(&data).map_err(|e| HaError::Local(e.to_string()))?,
            );
            json!({"type": kind, "encrypted": true, "encrypted_data": encrypt(&key, &plain)})
        }
        None => json!({"type": kind, "data": data}),
    };
    let url = join(base, &format!("/api/webhook/{}", reg.webhook_id.as_str()));
    let r = http.post(&url, None, Body::Json(&body)).await?;
    if r.status == 410 {
        return Err(HaError::Gone);
    }
    if !(200..300).contains(&r.status) {
        return Err(HaError::Http {
            status: r.status,
            message: error_message(&r),
        });
    }
    if r.json.get("encrypted").and_then(Value::as_bool) == Some(true) {
        let enc = r
            .json
            .get("encrypted_data")
            .and_then(Value::as_str)
            .unwrap_or("");
        let secret = reg.secret.as_deref().map(String::as_str).unwrap_or("");
        let plain = key_hex(secret)
            .and_then(|k| decrypt(&k, enc))
            .or_else(|| decrypt(&key_legacy(secret), enc))
            .ok_or_else(|| HaError::Protocol("cannot decrypt the webhook response".into()))?;
        return serde_json::from_slice(&plain).map_err(|e| HaError::Protocol(e.to_string()));
    }
    Ok(r.json)
}

/// Fire an event through the webhook (used for notification actions).
pub async fn fire_event(
    http: &Http,
    base: &Url,
    reg: &Registration,
    event_type: &str,
    event_data: Value,
) -> Result<(), HaError> {
    webhook(
        http,
        base,
        reg,
        "fire_event",
        json!({"event_type": event_type, "event_data": event_data}),
    )
    .await?;
    Ok(())
}

/// Update the registration (app version, device name, …).
pub async fn update_registration(
    http: &Http,
    base: &Url,
    reg: &Registration,
    info: &DeviceInfo,
) -> Result<(), HaError> {
    webhook(
        http,
        base,
        reg,
        "update_registration",
        json!({
            "app_version": hd_core::VERSION,
            "device_name": info.device_name,
            "manufacturer": info.manufacturer,
            "model": info.model,
            "os_version": info.os_version,
            "app_data": {"push_websocket_channel": true},
        }),
    )
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let secret = "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff";
        let key = key_hex(secret).unwrap();
        assert_eq!(key[1], 0x11);
        let enc = encrypt(&key, b"{\"a\":1}");
        assert_eq!(decrypt(&key, &enc).unwrap(), b"{\"a\":1}");
        assert!(decrypt(&key_legacy(secret), &enc).is_none());
        assert!(decrypt(&key, "AAAA").is_none());
        assert!(key_hex("zz").is_none());
    }

    #[test]
    fn pynacl_vector() {
        // Generated with PyNaCl (what Home Assistant uses):
        // SecretBox(key, encoder=HexEncoder).encrypt(b'{"event_type":"x"}', nonce=bytes([7]*24), encoder=Base64Encoder)
        let secret = "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff";
        let enc =
            "BwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHIXE1ed9pZMFZzdJCJU3CG+MNc0HOo5XEbrl6/uUTDi2i9A==";
        let key = key_hex(secret).unwrap();
        assert_eq!(decrypt(&key, enc).unwrap(), br#"{"event_type":"x"}"#);
        // And our encryption with the same nonce gives the same bytes.
        let ct = XSalsa20Poly1305::new(Key::from_slice(&*key))
            .encrypt(Nonce::from_slice(&[7u8; 24]), &br#"{"event_type":"x"}"#[..])
            .unwrap();
        let mut raw = vec![7u8; 24];
        raw.extend_from_slice(&ct);
        assert_eq!(base64::engine::general_purpose::STANDARD.encode(raw), enc);
    }

    #[test]
    fn legacy_key() {
        let k = key_legacy("abc");
        assert_eq!(&k[..3], b"abc");
        assert_eq!(k[3], 0);
    }
}
