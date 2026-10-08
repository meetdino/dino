//! Sign in with GitHub or Google: authorization code with PKCE and a state bound to the browser
//! session, then the provider's user API. Addresses and endpoints come from [`Config`], so tests
//! (and self-hosters) can point them elsewhere.

use serde::Deserialize;

use crate::AppState;
use crate::config::Upstream;
use crate::error::{Error, Result};
use crate::identity::Verified;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Provider {
    GitHub,
    Google,
}

impl Provider {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "github" => Some(Provider::GitHub),
            "google" => Some(Provider::Google),
            _ => None,
        }
    }

    pub fn id(self) -> &'static str {
        match self {
            Provider::GitHub => "github",
            Provider::Google => "google",
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Provider::GitHub => "GitHub",
            Provider::Google => "Google",
        }
    }

    pub fn config(self, s: &AppState) -> Option<&Upstream> {
        match self {
            Provider::GitHub => s.cfg.github.as_ref(),
            Provider::Google => s.cfg.google.as_ref(),
        }
    }

    fn scope(self) -> &'static str {
        match self {
            // Only the verified address list; nothing about repositories.
            Provider::GitHub => "read:user user:email",
            Provider::Google => "openid email",
        }
    }

    pub fn callback(self, s: &AppState) -> String {
        s.cfg.url(&format!("/signin/{}/callback", self.id()))
    }

    pub fn authorize_url(self, s: &AppState, state: &str, challenge: &str) -> Option<String> {
        let up = self.config(s)?;
        let mut u = url::Url::parse(&up.authorize_url).ok()?;
        u.query_pairs_mut()
            .append_pair("client_id", &up.client_id)
            .append_pair("redirect_uri", &self.callback(s))
            .append_pair("response_type", "code")
            .append_pair("scope", self.scope())
            .append_pair("state", state)
            .append_pair("code_challenge", challenge)
            .append_pair("code_challenge_method", "S256");
        if self == Provider::Google {
            u.query_pairs_mut().append_pair("prompt", "select_account");
        }
        Some(u.into())
    }

    /// Trade the provider's code for who the person is.
    pub async fn verify(self, s: &AppState, code: &str, verifier: &str) -> Result<Verified> {
        let up = self.config(s).ok_or(Error::NotFound)?;
        let fail = |what: &str| Error::Internal(anyhow::anyhow!("{} sign-in: {what}", self.name()));
        #[derive(Deserialize)]
        struct Token {
            access_token: Option<String>,
        }
        let token: Token = s
            .http
            .post(&up.token_url)
            .header("accept", "application/json")
            .form(&[
                ("grant_type", "authorization_code"),
                ("client_id", up.client_id.as_str()),
                ("client_secret", up.client_secret.as_str()),
                ("code", code),
                ("redirect_uri", self.callback(s).as_str()),
                ("code_verifier", verifier),
            ])
            .send()
            .await
            .map_err(|_| fail("token request failed"))?
            .json()
            .await
            .map_err(|_| fail("token response unreadable"))?;
        let access = token.access_token.ok_or_else(|| Error::Forbidden(format!("{} didn't sign you in. Try again.", self.name())))?;
        match self {
            Provider::GitHub => {
                #[derive(Deserialize)]
                struct User {
                    id: u64,
                }
                #[derive(Deserialize)]
                struct Email {
                    email: String,
                    primary: bool,
                    verified: bool,
                }
                let user: User = s
                    .http
                    .get(&up.userinfo_url)
                    .bearer_auth(&access)
                    .header("accept", "application/vnd.github+json")
                    .send()
                    .await
                    .map_err(|_| fail("user request failed"))?
                    .json()
                    .await
                    .map_err(|_| fail("user unreadable"))?;
                let emails: Vec<Email> = match &up.emails_url {
                    Some(u) => s
                        .http
                        .get(u)
                        .bearer_auth(&access)
                        .header("accept", "application/vnd.github+json")
                        .send()
                        .await
                        .map_err(|_| fail("emails request failed"))?
                        .json()
                        .await
                        .map_err(|_| fail("emails unreadable"))?,
                    None => vec![],
                };
                let best = emails.iter().find(|e| e.primary && e.verified).or_else(|| emails.iter().find(|e| e.verified));
                Ok(Verified { provider: "github", subject: user.id.to_string(), email: best.map(|e| e.email.clone()), email_verified: best.is_some() })
            }
            Provider::Google => {
                #[derive(Deserialize)]
                struct User {
                    sub: String,
                    email: Option<String>,
                    #[serde(default)]
                    email_verified: bool,
                }
                let user: User = s.http.get(&up.userinfo_url).bearer_auth(&access).send().await.map_err(|_| fail("userinfo request failed"))?.json().await.map_err(|_| fail("userinfo unreadable"))?;
                Ok(Verified { provider: "google", subject: user.sub, email_verified: user.email_verified && user.email.is_some(), email: user.email })
            }
        }
    }
}
