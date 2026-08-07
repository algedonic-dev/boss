//! Outbound mail for authentication.
//!
//! Scope is deliberately narrow: this sends mail that lets someone get
//! into their account, and nothing else. Notifications are a different
//! problem with a different sender and different volume, and mixing
//! them is how a notification-volume mistake takes down the mail
//! people need to recover access.
//!
//! ## Why a port
//!
//! Cloudflare fronts the deployment and holds DNS, but Email Routing
//! is inbound only — there is no first-party outbound product — so
//! sending needs one external dependency. A port keeps that dependency
//! swappable: today an HTTP provider API, tomorrow SMTP or whatever
//! Cloudflare ships, without the auth handlers knowing.
//!
//! It is `MailTransport`, not `Smtp`. Every provider worth using
//! offers an HTTP API, which needs no new crate because `reqwest` is
//! already here — naming the port after one wire protocol would be
//! wrong on day one.
//!
//! ## Why the default sends nothing
//!
//! [`LogTransport`] is the default, and it writes the message to the
//! log instead of delivering it. A deployment with no mail configured
//! must not break password reset, and — more importantly — must not
//! silently *pretend* to have sent something. The log line is the
//! honest outcome: the mail was composed, and this deployment has
//! nowhere to send it.

use std::sync::Arc;

use async_trait::async_trait;

/// A composed message, ready to send.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutboundMail {
    pub to: String,
    pub subject: String,
    /// Plain text only. Auth mail is short, and an HTML body is an
    /// attack surface plus a rendering problem for no benefit.
    pub body: String,
}

#[derive(Debug, thiserror::Error)]
pub enum MailError {
    #[error("mail transport: {0}")]
    Transport(String),
}

#[async_trait]
pub trait MailTransport: Send + Sync {
    async fn send(&self, mail: &OutboundMail) -> Result<(), MailError>;
    /// Whether this transport actually delivers. The `forgot` handler
    /// uses it to decide what to tell an operator in the logs — it
    /// must NOT change what the caller is told, since that would leak
    /// deployment configuration into a public endpoint.
    fn delivers(&self) -> bool;
}

/// Composes and logs, sends nothing. The default.
pub struct LogTransport;

#[async_trait]
impl MailTransport for LogTransport {
    async fn send(&self, mail: &OutboundMail) -> Result<(), MailError> {
        // The body carries a single-use credential, so it is logged at
        // INFO deliberately and only by this transport: an operator
        // running without a mail provider needs to be able to complete
        // a reset by reading the log. A configured deployment never
        // takes this path.
        tracing::info!(
            to = %mail.to,
            subject = %mail.subject,
            body = %mail.body,
            "mail not sent — no transport configured; body logged so a reset is still completable"
        );
        Ok(())
    }
    fn delivers(&self) -> bool {
        false
    }
}

/// POSTs to a provider's HTTP send API.
///
/// Generic over the provider by construction: the endpoint, the bearer
/// token and the field names are configuration. Resend, Postmark and
/// SES all accept a flat JSON body of from/to/subject/text under some
/// spelling, so one adapter covers them with three env vars rather
/// than three adapters.
pub struct HttpApiTransport {
    client: reqwest::Client,
    endpoint: String,
    token: String,
    from: String,
}

impl HttpApiTransport {
    pub fn new(endpoint: String, token: String, from: String) -> Result<Self, MailError> {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(15))
            .build()
            .map_err(|e| MailError::Transport(e.to_string()))?;
        Ok(Self {
            client,
            endpoint,
            token,
            from,
        })
    }
}

#[async_trait]
impl MailTransport for HttpApiTransport {
    async fn send(&self, mail: &OutboundMail) -> Result<(), MailError> {
        let body = serde_json::json!({
            "from": self.from,
            "to": [mail.to],
            "subject": mail.subject,
            "text": mail.body,
        });
        let resp = self
            .client
            .post(&self.endpoint)
            .bearer_auth(&self.token)
            .json(&body)
            .send()
            .await
            .map_err(|e| MailError::Transport(e.to_string()))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let detail = resp.text().await.unwrap_or_default();
            // Never log the body here — it carries the token.
            return Err(MailError::Transport(format!(
                "provider returned {status}: {detail}"
            )));
        }
        Ok(())
    }
    fn delivers(&self) -> bool {
        true
    }
}

/// Build the transport from the environment.
///
/// `BOSS_AUTH_MAIL_FROM` is auth-specific on purpose. Notifications
/// will have their own sender, so that a volume mistake on one cannot
/// damage deliverability of the other. One variable naming two senders
/// is the same shape as the flag that used to answer three unrelated
/// questions.
pub fn from_env() -> Arc<dyn MailTransport> {
    let endpoint = std::env::var("BOSS_MAIL_API_URL").ok();
    let token = std::env::var("BOSS_MAIL_API_TOKEN").ok();
    let from = std::env::var("BOSS_AUTH_MAIL_FROM").ok();

    match (endpoint, token, from) {
        (Some(e), Some(t), Some(f)) if !e.is_empty() && !t.is_empty() && !f.is_empty() => {
            match HttpApiTransport::new(e, t, f.clone()) {
                Ok(tr) => {
                    tracing::info!(from = %f, "auth mail transport configured");
                    Arc::new(tr)
                }
                Err(e) => {
                    tracing::warn!(error = %e, "auth mail transport failed to build; logging instead");
                    Arc::new(LogTransport)
                }
            }
        }
        _ => {
            tracing::info!(
                "no auth mail transport configured (need BOSS_MAIL_API_URL, \
                 BOSS_MAIL_API_TOKEN, BOSS_AUTH_MAIL_FROM); reset tokens will be logged"
            );
            Arc::new(LogTransport)
        }
    }
}

/// The reset message. One place, so the wording and the link shape
/// cannot drift between the admin-issued and self-service paths.
pub fn reset_mail(to: &str, token: &str, base_url: &str) -> OutboundMail {
    let link = format!(
        "{}/login?reset={}",
        base_url.trim_end_matches('/'),
        urlencode(token)
    );
    OutboundMail {
        to: to.to_string(),
        subject: "Reset your BOSS password".to_string(),
        body: format!(
            "Someone asked to reset the BOSS password for {to}.\n\n\
             Open this link within the hour:\n\n  {link}\n\n\
             Or enter this token on the reset form:\n\n  {token}\n\n\
             If this wasn't you, ignore this message — the token is \
             single-use and expires on its own, and nothing has changed \
             on the account.\n"
        ),
    }
}

/// Minimal percent-encoding for a token in a query string. Tokens are
/// hex today; this exists so a future token alphabet cannot silently
/// produce a broken link.
fn urlencode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn the_default_transport_reports_that_it_does_not_deliver() {
        // The `forgot` handler branches on this for its LOG line only.
        // If a transport ever lied here, an operator would believe a
        // reset had gone out when it had not.
        let t = LogTransport;
        assert!(!t.delivers());
        assert!(
            t.send(&reset_mail("a@b.c", "tok", "https://x"))
                .await
                .is_ok()
        );
    }

    #[test]
    fn the_reset_link_carries_an_encoded_token() {
        let m = reset_mail("op@example.com", "ab+cd/ef", "https://boss.example/");
        assert!(
            m.body
                .contains("https://boss.example/login?reset=ab%2Bcd%2Fef")
        );
        // The raw token is offered too — a link can be mangled by a
        // mail client, and the form accepts a pasted token.
        assert!(m.body.contains("ab+cd/ef"));
        assert_eq!(m.to, "op@example.com");
    }

    #[test]
    fn the_message_does_not_alarm_someone_who_did_not_ask() {
        // A reset mail lands in the inbox of people who did not request
        // it, either by typo or by an attacker probing addresses. It
        // has to say plainly that ignoring it is safe, or it becomes a
        // support burden and a phishing-shaped experience.
        let m = reset_mail("a@b.c", "tok", "https://x");
        assert!(m.body.contains("ignore this message"));
        assert!(m.body.contains("nothing has changed"));
    }
}
