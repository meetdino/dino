//! The clients allowed to ask for tokens. All first-party and public (no secret can be kept in an
//! app people download), so PKCE is required for every one of them.

use url::Url;

use crate::config::Config;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    /// A desktop app or CLI: loopback redirects on any port (RFC 8252 §7.3), and the device flow.
    Native,
    /// A web page served from a fixed address.
    Web,
}

#[derive(Clone, Copy, Debug)]
pub struct Client {
    pub id: &'static str,
    /// Shown on consent screens.
    pub name: &'static str,
    /// Tokens issued to this client name it as their audience; each product checks for its own.
    pub aud: &'static str,
    pub kind: Kind,
    /// Native: the loopback path. Web: the path on the server's own address.
    pub redirect_path: &'static str,
}

pub const CLIENTS: &[Client] = &[
    Client { id: "dino", name: "dino", aud: "dino", kind: Kind::Native, redirect_path: "/callback" },
    Client { id: "dino-harness", name: "dino harness", aud: "dino-harness", kind: Kind::Native, redirect_path: "/callback" },
    Client { id: "dino-web", name: "dino account", aud: "dino", kind: Kind::Web, redirect_path: "/account/callback" },
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
    /// Whether `uri` is a redirect this client registered. Native: `http` to a loopback IP literal
    /// (not `localhost`, RFC 8252 §8.3), any port, the registered path, no fragment. Web: exact.
    pub fn allows_redirect(&self, cfg: &Config, uri: &str) -> bool {
        match self.kind {
            Kind::Web => uri == cfg.url(self.redirect_path),
            Kind::Native => {
                let Ok(u) = Url::parse(uri) else { return false };
                let loopback = matches!(u.host(), Some(url::Host::Ipv4(ip)) if ip.is_loopback()) || matches!(u.host(), Some(url::Host::Ipv6(ip)) if ip.is_loopback());
                u.scheme() == "http" && loopback && u.path() == self.redirect_path && u.fragment().is_none() && u.username().is_empty() && u.password().is_none()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> Config {
        Config {
            public_url: "https://account.example".parse().unwrap(),
            bind: "127.0.0.1:0".parse().unwrap(),
            metrics_bind: None,
            database_url: String::new(),
            secret: [0; 32],
            trust_proxy: false,
            github: None,
            google: None,
            mail: crate::config::MailConfig::File("/dev/null".into()),
            turnstile: None,
            introspect_secret: None,
            json_logs: false,
        }
    }

    #[test]
    fn native_redirects_are_loopback_ip_literals_on_the_registered_path() {
        let c = find("dino").unwrap();
        let cfg = cfg();
        assert!(c.allows_redirect(&cfg, "http://127.0.0.1:49152/callback"));
        assert!(c.allows_redirect(&cfg, "http://[::1]:8080/callback"));
        assert!(!c.allows_redirect(&cfg, "http://localhost:49152/callback"));
        assert!(!c.allows_redirect(&cfg, "https://evil.example/callback"));
        assert!(!c.allows_redirect(&cfg, "http://127.0.0.1:1/other"));
        assert!(!c.allows_redirect(&cfg, "http://127.0.0.1:1/callback#frag"));
        assert!(!c.allows_redirect(&cfg, "http://user@127.0.0.1:1/callback"));
        let w = find("dino-web").unwrap();
        assert!(w.allows_redirect(&cfg, "https://account.example/account/callback"));
        assert!(!w.allows_redirect(&cfg, "https://account.example/account/callback?x=1"));
    }

    #[test]
    fn scopes_are_checked() {
        assert_eq!(scope(None).as_deref(), Some("account"));
        assert_eq!(scope(Some("openid  email")).as_deref(), Some("openid email"));
        assert_eq!(scope(Some("admin")), None);
    }
}
