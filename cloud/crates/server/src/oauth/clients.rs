//! The clients allowed to ask for tokens. All first-party and public (no secret can be kept in an
//! app people download), so PKCE is required for every one of them.

use url::Url;

#[derive(Clone, Copy, Debug)]
pub struct Client {
    pub id: &'static str,
    /// Shown on consent screens.
    pub name: &'static str,
    /// Tokens issued to this client name it as their audience; each product checks for its own.
    pub aud: &'static str,
    /// The loopback path: each is a desktop app or CLI, redirected to on any port (RFC 8252 §7.3).
    pub redirect_path: &'static str,
}

pub const CLIENTS: &[Client] = &[
    Client { id: "dino", name: "dino", aud: "dino", redirect_path: "/callback" },
    Client { id: "dino-harness", name: "dino harness", aud: "dino-harness", redirect_path: "/callback" },
];

pub const SCOPES: &[&str] = &["openid", "email", "account", "sync"];

pub fn find(id: &str) -> Option<&'static Client> {
    CLIENTS.iter().find(|c| c.id == id)
}

/// Every scope asked for is one we know; none asked for means `account`.
pub fn scope(requested: Option<&str>) -> Option<String> {
    let requested = requested.map(str::trim).filter(|s| !s.is_empty()).unwrap_or("account");
    requested.split(' ').filter(|s| !s.is_empty()).all(|s| SCOPES.contains(&s)).then(|| requested.split_whitespace().collect::<Vec<_>>().join(" "))
}

impl Client {
    /// Whether `uri` is a redirect this client registered: `http` to the loopback IP literal
    /// 127.0.0.1 (not `localhost`, RFC 8252 §8.3), any port, the registered path, no fragment. Not
    /// `[::1]`: browsers don't accept IPv6 literals in the CSP that lets the consent form land there.
    pub fn allows_redirect(&self, uri: &str) -> bool {
        let Ok(u) = Url::parse(uri) else { return false };
        let loopback = matches!(u.host(), Some(url::Host::Ipv4(ip)) if ip == std::net::Ipv4Addr::LOCALHOST);
        u.scheme() == "http" && loopback && u.path() == self.redirect_path && u.fragment().is_none() && u.username().is_empty() && u.password().is_none()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_redirects_are_loopback_ip_literals_on_the_registered_path() {
        let c = find("dino").unwrap();
        assert!(c.allows_redirect("http://127.0.0.1:49152/callback"));
        assert!(!c.allows_redirect("http://[::1]:8080/callback"));
        assert!(!c.allows_redirect("http://localhost:49152/callback"));
        assert!(!c.allows_redirect("https://evil.example/callback"));
        assert!(!c.allows_redirect("http://127.0.0.1:1/other"));
        assert!(!c.allows_redirect("http://127.0.0.1:1/callback#frag"));
        assert!(!c.allows_redirect("http://user@127.0.0.1:1/callback"));
    }

    #[test]
    fn scopes_are_checked() {
        assert_eq!(scope(None).as_deref(), Some("account"));
        assert_eq!(scope(Some("openid  email")).as_deref(), Some("openid email"));
        assert_eq!(scope(Some("admin")), None);
    }
}
