//! End-to-end test against a real (throwaway!) Home Assistant instance.
//!
//! Ignored by default. Start a test instance (see AGENTS.md, "Live test")
//! and run:
//!
//! ```sh
//! HD_TEST_HA_URL=http://127.0.0.1:8123 cargo test -p hd-ha --test live_ha -- --ignored --nocapture
//! ```
//!
//! The test onboards the instance if needed (admin user `admin`), creates a
//! non-admin user `tray`, and then exercises everything hassdash4linux does
//! as that non-admin user: browser login with PKCE (the browser is simulated
//! through the login flow API), WebSocket commands and subscriptions,
//! mobile_app registration, push notifications and encrypted webhooks,
//! long-lived tokens and token revocation.

use std::time::Duration;

use serde_json::json;
use url::Url;

use hd_ha::http::{Body, Http};
use hd_ha::mobile_app::{self, DeviceInfo};
use hd_ha::{Connector, Credentials, EntityCache, HaError, Session};

const ADMIN: (&str, &str) = ("admin", "admin-password-1");
const TRAY: (&str, &str) = ("tray", "tray-password-1");
const DEVICE_ID: &str = "0b9b2c1e-1f0a-4e55-9d1b-7f1c2a3b4c5d";

fn base() -> Option<Url> {
    std::env::var("HD_TEST_HA_URL")
        .ok()
        .map(|u| hd_core::server::normalize(&u).expect("HD_TEST_HA_URL"))
}

fn join(b: &Url, p: &str) -> Url {
    hd_core::server::join(b, p)
}

async fn timeout<T>(what: &str, f: impl std::future::Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(20), f)
        .await
        .unwrap_or_else(|_| panic!("timeout: {what}"))
}

/// Drive the login form like the Home Assistant frontend does, then follow
/// the redirect to our loopback listener.
async fn simulated_browser(http: Http, base: Url, authorize: Url, user: (&str, &str)) {
    let q: std::collections::HashMap<String, String> =
        authorize.query_pairs().into_owned().collect();
    let flow = http
        .post(
            &join(&base, "/auth/login_flow"),
            None,
            Body::Json(&json!({
                "client_id": q["client_id"],
                "handler": ["homeassistant", null],
                "redirect_uri": q["redirect_uri"],
                "code_challenge": q["code_challenge"],
                "code_challenge_method": q["code_challenge_method"],
            })),
        )
        .await
        .unwrap();
    assert_eq!(flow.status, 200, "{:?}", flow.json);
    let flow_id = flow.json["flow_id"].as_str().unwrap().to_owned();
    let done = http
        .post(
            &join(&base, &format!("/auth/login_flow/{flow_id}")),
            None,
            Body::Json(
                &json!({"client_id": q["client_id"], "username": user.0, "password": user.1}),
            ),
        )
        .await
        .unwrap();
    assert_eq!(done.json["type"], "create_entry", "{:?}", done.json);
    let code = done.json["result"].as_str().unwrap();
    let mut redirect = Url::parse(&q["redirect_uri"]).unwrap();
    redirect
        .query_pairs_mut()
        .append_pair("code", code)
        .append_pair("state", &q["state"]);
    let r = http.get(&redirect, None).await.unwrap();
    assert_eq!(r.status, 200);
}

async fn oauth(
    http: &Http,
    base: &Url,
    user: (&'static str, &'static str),
) -> hd_ha::auth::OAuthLogin {
    let (h, b) = (http.clone(), base.clone());
    hd_ha::auth::oauth_login(http, base, move |url| {
        tokio::spawn(simulated_browser(h, b, url, user));
    })
    .await
    .expect("oauth login")
}

/// Onboard a fresh instance (creating the admin user) or log in as admin.
async fn admin_session(http: &Http, base: &Url) -> Session {
    let onboarding = http
        .get(&join(base, "/api/onboarding"), None)
        .await
        .unwrap();
    let user_done = onboarding.json.as_array().map(|steps| {
        steps
            .iter()
            .any(|s| s["step"] == "user" && s["done"] == true)
    });
    let token = if user_done == Some(false) {
        let client_id = format!("{}/", base.as_str().trim_end_matches('/'));
        let r = http
            .post(
                &join(base, "/api/onboarding/users"),
                None,
                Body::Json(&json!({"client_id": client_id, "name": "Admin", "username": ADMIN.0, "password": ADMIN.1, "language": "en"})),
            )
            .await
            .unwrap();
        let code = r.json["auth_code"].as_str().expect("auth_code").to_owned();
        let t = http
            .post(
                &join(base, "/auth/token"),
                None,
                Body::Form(&[
                    ("grant_type", "authorization_code"),
                    ("code", &code),
                    ("client_id", &client_id),
                ]),
            )
            .await
            .unwrap();
        let token = t.json["access_token"].as_str().unwrap().to_owned();
        for (step, body) in [
            ("core_config", json!({})),
            ("analytics", json!({})),
            (
                "integration",
                json!({"client_id": client_id, "redirect_uri": format!("{client_id}?auth_callback=1")}),
            ),
        ] {
            let _ = http
                .post(
                    &join(base, &format!("/api/onboarding/{step}")),
                    Some(&token),
                    Body::Json(&body),
                )
                .await;
        }
        token
    } else {
        oauth(http, base, ADMIN)
            .await
            .tokens
            .access_token
            .to_string()
    };
    Session::connect(base, http.tls(), &token)
        .await
        .expect("admin session")
}

async fn ensure_tray_user(admin: &Session) {
    let users = admin
        .call(json!({"type": "config/auth/list"}))
        .await
        .unwrap();
    let exists = users
        .as_array()
        .unwrap()
        .iter()
        .any(|u| u["username"] == TRAY.0);
    if exists {
        return;
    }
    let user = admin
        .call(json!({"type": "config/auth/create", "name": "Tray", "group_ids": ["system-users"], "local_only": false}))
        .await
        .unwrap();
    let id = user["user"]["id"].as_str().unwrap();
    admin
        .call(json!({"type": "config/auth_provider/homeassistant/create", "user_id": id, "username": TRAY.0, "password": TRAY.1}))
        .await
        .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "needs a throwaway Home Assistant instance (HD_TEST_HA_URL)"]
async fn live() {
    let Some(base) = base() else {
        eprintln!("HD_TEST_HA_URL not set; skipping");
        return;
    };
    let tls = hd_ha::tls::build(None).unwrap();
    let http = Http::new(&tls).unwrap();
    let admin = admin_session(&http, &base).await;
    hd_ha::auth::check_server(&http, &base)
        .await
        .expect("check_server");
    ensure_tray_user(&admin).await;
    eprintln!(
        "✓ onboarding / admin login, Home Assistant {}",
        admin.ha_version()
    );

    // --- Browser login (OAuth + PKCE) as non-admin user -------------------
    let login = oauth(&http, &base, TRAY).await;
    assert!(login.client_id.starts_with("http://127.0.0.1:"));
    let refresh = login.tokens.refresh_token.clone().expect("refresh token");
    let connector = Connector::new(
        base.clone(),
        None,
        Credentials::OAuth {
            client_id: login.client_id.clone(),
            refresh_token: refresh.clone(),
        },
    )
    .unwrap();
    let session = connector.connect().await.expect("connect as tray user");
    eprintln!("✓ browser login with PKCE as non-admin user");

    // --- Commands available to non-admin users ----------------------------
    let config = session.get_config().await.unwrap();
    assert!(config.get("location_name").is_some());
    let states = session.get_states().await.unwrap();
    let light = states
        .iter()
        .find(|s| {
            s.domain() == "light"
                && s.attributes.get("supported_color_modes").is_some_and(|m| {
                    m.to_string().contains("color_temp") || m.to_string().contains("hs")
                })
        })
        .or_else(|| states.iter().find(|s| s.domain() == "light"))
        .expect("a light (demo integration)")
        .entity_id
        .clone();
    assert!(
        states.iter().any(|s| s.domain() == "automation"),
        "an automation"
    );
    assert!(states.iter().any(|s| s.domain() == "scene"), "a scene");
    assert!(states.iter().any(|s| s.domain() == "sensor"), "a sensor");
    for (name, r) in [
        ("entity registry", session.entity_registry_display().await),
        ("area registry", session.area_registry().await),
        ("device registry", session.device_registry().await),
    ] {
        eprintln!(
            "  {name} as non-admin: {}",
            if r.is_ok() {
                "ok"
            } else {
                "not permitted (handled)"
            }
        );
    }
    eprintln!("✓ get_config / get_states ({} entities)", states.len());

    // --- subscribe_entities + call_service --------------------------------
    let mut sub = session
        .subscribe_entities(std::slice::from_ref(&light))
        .await
        .unwrap();
    let mut cache = EntityCache::new();
    cache.apply(&timeout("initial state", sub.next()).await.unwrap());
    assert!(cache.get(&light).is_some());
    let mut data = serde_json::Map::new();
    data.insert("brightness_pct".into(), json!(40));
    session
        .call_service("light", "turn_on", &light, data)
        .await
        .unwrap();
    timeout("brightness change", async {
        loop {
            cache.apply(&sub.next().await.unwrap());
            let st = cache.get(&light).unwrap();
            if st.is_on() && hd_ha::format::brightness_percent(st) == Some(40) {
                break;
            }
        }
    })
    .await;
    drop(sub);
    eprintln!("✓ subscribe_entities + call_service ({light} at 40 %)");

    // Services outside the allowlist are never called by the app, but a
    // non-admin user can still call them: the allowlist matters.
    assert!(!hd_core::entry::entity_service_allowed(
        "homeassistant",
        "restart"
    ));

    // --- Companion registration -------------------------------------------
    let info = DeviceInfo::collect(DEVICE_ID, "hd test device");
    let token = connector.access_token().await.unwrap();
    let reg = mobile_app::register(&connector.http, &base, &token, &info)
        .await
        .expect("register");
    assert!(reg.secret.is_some(), "encryption secret");
    mobile_app::update_registration(&connector.http, &base, &reg, &info)
        .await
        .expect("update_registration (encrypted request + response)");
    eprintln!("✓ mobile_app registration + encrypted update_registration");

    // --- Push notification channel -----------------------------------------
    let mut push = session
        .subscribe(json!({"type": "mobile_app/push_notification_channel", "webhook_id": reg.webhook_id.as_str(), "support_confirm": true}))
        .await
        .expect("push channel");
    let s2 = session.clone();
    let notify = tokio::spawn(async move {
        let mut d = serde_json::Map::new();
        d.insert("message".into(), json!("Hello from the live test"));
        d.insert("title".into(), json!("hassdash4linux"));
        d.insert(
            "data".into(),
            json!({"tag": "live", "actions": [{"action": "OK", "title": "OK"}]}),
        );
        s2.call(json!({"type": "call_service", "domain": "notify", "service": "mobile_app_hd_test_device", "service_data": d})).await
    });
    let ev = timeout("push notification", push.next()).await.unwrap();
    assert_eq!(ev["message"], "Hello from the live test");
    let confirm = ev["hass_confirm_id"].as_str().expect("hass_confirm_id");
    session.send(json!({"type": "mobile_app/push_notification_confirm", "webhook_id": reg.webhook_id.as_str(), "confirm_id": confirm}));
    notify.await.unwrap().expect("notify service call");
    eprintln!("✓ push notification received and confirmed: {ev}");

    // --- Notification action via encrypted webhook --------------------------
    let mut events = admin
        .subscribe(
            json!({"type": "subscribe_events", "event_type": "mobile_app_notification_action"}),
        )
        .await
        .unwrap();
    mobile_app::fire_event(
        &connector.http,
        &base,
        &reg,
        "mobile_app_notification_action",
        json!({"action": "OK", "tag": "live"}),
    )
    .await
    .expect("fire_event");
    let ev = timeout("notification action event", events.next())
        .await
        .unwrap();
    assert_eq!(ev["data"]["action"], "OK");
    eprintln!("✓ encrypted webhook fire_event arrived in Home Assistant");

    // --- Long-lived token ----------------------------------------------------
    let llat = session
        .call(json!({"type": "auth/long_lived_access_token", "client_name": format!("hd-live-{}", hd_core::rand::token(4)), "lifespan": 1}))
        .await
        .expect("create long-lived token");
    let llc = Connector::new(
        base.clone(),
        None,
        Credentials::LongLived(llat.as_str().unwrap().to_owned().into()),
    )
    .unwrap();
    let s3 = llc.connect().await.expect("connect with long-lived token");
    s3.get_config().await.unwrap();
    eprintln!("✓ long-lived access token");

    // --- Wrong credentials and revocation -------------------------------------
    let bad = Connector::new(
        base.clone(),
        None,
        Credentials::LongLived("nope".to_owned().into()),
    )
    .unwrap();
    assert!(matches!(bad.connect().await, Err(HaError::Auth(_))));
    hd_ha::auth::revoke(&http, &base, &refresh).await.unwrap();
    let r = hd_ha::auth::refresh(&http, &base, &login.client_id, &refresh).await;
    assert!(
        matches!(r, Err(HaError::Auth(_))),
        "refresh after revoke: {r:?}"
    );
    eprintln!("✓ invalid token rejected, refresh token revoked");
}
