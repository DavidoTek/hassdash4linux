//! Validation and normalisation of Home Assistant server URLs.

use std::net::IpAddr;
use url::{Host, Url};

/// Why a server URL was rejected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UrlError {
    /// Not a URL at all.
    Invalid,
    /// Only `http` and `https` are supported.
    Scheme,
    /// User names, passwords, queries and fragments are not allowed.
    ExtraParts,
    /// Plain HTTP is only allowed for local hosts.
    InsecurePublicHost,
}

impl std::fmt::Display for UrlError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        use crate::i18n::tr;
        f.write_str(match self {
            UrlError::Invalid => tr("This is not a valid address."),
            UrlError::Scheme => tr("Only http:// and https:// addresses are supported."),
            UrlError::ExtraParts => {
                tr("The address must not contain a user name, password, query or fragment.")
            }
            UrlError::InsecurePublicHost => tr(
                "Unencrypted http:// is only allowed for addresses in your local network. Use https:// for this server.",
            ),
        })
    }
}

impl std::error::Error for UrlError {}

/// Parse and normalise a user-entered server address.
///
/// * a missing scheme defaults to `http://` for local hosts and `https://` otherwise,
/// * the result has no trailing slash, query, fragment or credentials,
/// * plain HTTP is only accepted for local hosts (see [`is_local_host`]).
pub fn normalize(input: &str) -> Result<Url, UrlError> {
    let input = input.trim();
    if input.is_empty() {
        return Err(UrlError::Invalid);
    }
    let with_scheme = if input.contains("://") {
        input.to_owned()
    } else {
        // A mistyped scheme (`https:/host`, `https//host`) must not silently
        // become plain HTTP to a host named `https`.
        let lower = input.to_ascii_lowercase();
        if ["http", "https"].iter().any(|s| {
            lower
                .strip_prefix(s)
                .is_some_and(|rest| rest.starts_with([':', '/']))
        }) {
            return Err(UrlError::Invalid);
        }
        // Decide on a default scheme based on the host.
        let probe = Url::parse(&format!("http://{input}")).map_err(|_| UrlError::Invalid)?;
        if is_local_host(&probe) {
            format!("http://{input}")
        } else {
            format!("https://{input}")
        }
    };
    let mut url = Url::parse(&with_scheme).map_err(|_| UrlError::Invalid)?;
    match url.scheme() {
        "http" | "https" => {}
        _ => return Err(UrlError::Scheme),
    }
    if url.host().is_none() {
        return Err(UrlError::Invalid);
    }
    if !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(UrlError::ExtraParts);
    }
    if url.scheme() == "http" && !is_local_host(&url) {
        return Err(UrlError::InsecurePublicHost);
    }
    // Keep a sub path (reverse proxies), but without trailing slash.
    let path = url.path().trim_end_matches('/').to_owned();
    url.set_path(&path);
    Ok(url)
}

/// Re-validate a URL loaded from the configuration file.
pub fn validate(url: &str) -> Result<Url, UrlError> {
    let parsed = Url::parse(url).map_err(|_| UrlError::Invalid)?;
    let normalized = normalize(url)?;
    if parsed.scheme() != normalized.scheme() {
        return Err(UrlError::Invalid);
    }
    Ok(normalized)
}

/// The canonical string form of a normalised URL, as stored in the
/// configuration and used as the keyring's `server` attribute.
pub fn canonical(url: &Url) -> String {
    url.as_str().trim_end_matches('/').to_owned()
}

/// Whether the URL's host is on the local machine or the local network:
/// loopback, private and link-local IPs, single-label host names and the
/// `.local`, `.lan`, `.home.arpa`, `.internal` and `.localdomain` suffixes.
pub fn is_local_host(url: &Url) -> bool {
    match url.host() {
        None => false,
        Some(Host::Ipv4(ip)) => is_local_ip(IpAddr::V4(ip)),
        Some(Host::Ipv6(ip)) => is_local_ip(IpAddr::V6(ip)),
        Some(Host::Domain(d)) => {
            let d = d.trim_end_matches('.').to_ascii_lowercase();
            if d == "localhost" || !d.contains('.') {
                return true;
            }
            [
                ".local",
                ".lan",
                ".home.arpa",
                ".internal",
                ".localdomain",
                ".localhost",
            ]
            .iter()
            .any(|s| d.ends_with(s))
        }
    }
}

fn is_local_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.is_loopback() || v4.is_private() || v4.is_link_local(),
        IpAddr::V6(v6) => {
            let seg0 = v6.segments()[0];
            v6.is_loopback()
                || (seg0 & 0xfe00) == 0xfc00 // unique local fc00::/7
                || (seg0 & 0xffc0) == 0xfe80 // link local fe80::/10
                || v6.to_ipv4_mapped().is_some_and(|v4| is_local_ip(IpAddr::V4(v4)))
        }
    }
}

/// Join an API path (starting with `/`) to the server base URL.
pub fn join(base: &Url, path: &str) -> Url {
    let mut u = base.clone();
    let p = format!("{}{}", base.path().trim_end_matches('/'), path);
    u.set_path(&p);
    u
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_and_policy() {
        assert_eq!(
            normalize("homeassistant.local:8123").unwrap().as_str(),
            "http://homeassistant.local:8123/"
        );
        assert_eq!(
            normalize("192.168.1.10:8123").unwrap().as_str(),
            "http://192.168.1.10:8123/"
        );
        assert_eq!(
            normalize("ha.example.org").unwrap().as_str(),
            "https://ha.example.org/"
        );
        assert_eq!(
            normalize("http://ha.example.org"),
            Err(UrlError::InsecurePublicHost)
        );
        assert_eq!(
            normalize("http://8.8.8.8"),
            Err(UrlError::InsecurePublicHost)
        );
        assert!(normalize("http://[fd00::1]:8123").is_ok());
        assert!(normalize("http://homeassistant:8123").is_ok());
        assert_eq!(normalize("ftp://x.local"), Err(UrlError::Scheme));
        assert_eq!(
            normalize("https://u:p@ha.example.org"),
            Err(UrlError::ExtraParts)
        );
        assert_eq!(
            normalize("https://ha.example.org/?a=b"),
            Err(UrlError::ExtraParts)
        );
        assert_eq!(normalize(""), Err(UrlError::Invalid));
        assert_eq!(
            normalize("https://ha.example.org/sub/").unwrap().as_str(),
            "https://ha.example.org/sub"
        );
    }

    #[test]
    fn mistyped_scheme_is_rejected() {
        for input in [
            "https:/ha.example.org",
            "https//ha.example.org",
            "HTTP:ha.local",
        ] {
            assert_eq!(normalize(input), Err(UrlError::Invalid), "{input}");
        }
        assert!(normalize("httpserver.local:8123").is_ok());
    }

    #[test]
    fn canonical_form() {
        let u = normalize("https://HA.example.org/").unwrap();
        assert_eq!(canonical(&u), "https://ha.example.org");
        let u = normalize("https://ha.example.org/sub/").unwrap();
        assert_eq!(canonical(&u), "https://ha.example.org/sub");
    }

    #[test]
    fn join_paths() {
        let b = normalize("https://ha.example.org").unwrap();
        assert_eq!(
            join(&b, "/api/websocket").as_str(),
            "https://ha.example.org/api/websocket"
        );
        let b = normalize("https://ha.example.org/sub").unwrap();
        assert_eq!(
            join(&b, "/auth/token").as_str(),
            "https://ha.example.org/sub/auth/token"
        );
    }
}
