//! Sending sign-in codes. In development they go to a file (never to stdout, where they'd land in
//! logs); in production to a transactional mail API.

use std::io::Write;

use crate::config::MailConfig;

pub enum Mailer {
    File(std::path::PathBuf),
    Http { http: reqwest::Client, url: String, key: String, from: String },
}

impl Mailer {
    pub fn new(cfg: &MailConfig, http: reqwest::Client) -> Self {
        match cfg {
            MailConfig::File(p) => Mailer::File(p.clone()),
            MailConfig::Http { url, key, from } => Mailer::Http { http, url: url.clone(), key: key.clone(), from: from.clone() },
        }
    }

    pub async fn send(&self, to: &str, subject: &str, text: &str) -> anyhow::Result<()> {
        match self {
            Mailer::File(path) => {
                let mut f = std::fs::OpenOptions::new().create(true).append(true).open(path)?;
                writeln!(f, "To: {to}\nSubject: {subject}\n\n{text}\n---")?;
                Ok(())
            }
            Mailer::Http { http, url, key, from } => {
                let r = http.post(url).bearer_auth(key).json(&serde_json::json!({"from": from, "to": [to], "subject": subject, "text": text})).send().await?;
                anyhow::ensure!(r.status().is_success(), "mail API answered {}", r.status());
                Ok(())
            }
        }
    }
}
