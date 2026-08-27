//! A report from a church, sent to us rather than to their mail client.
//!
//! # Why this is posted and not a mailto: link
//!
//! A church machine with no mail client configured is exactly the one most
//! likely to have something worth reporting, and there a link does nothing at
//! all. The operator is also frequently not the person whose email account is
//! on the machine -- it is a shared desk at the back of a hall -- so opening
//! "their" mail is opening somebody else's.
//!
//! # Why the failure is returned rather than swallowed
//!
//! Unlike the check-in and the tally, somebody is watching this one: they
//! pressed send. A silent failure leaves a church believing they reported
//! something nobody will ever read, which is worse than telling them it did not
//! go.

use anyhow::{bail, Context, Result};

/// Long enough for a hall's connection, short enough that nobody is left
/// watching a spinner wondering whether to press it again.
const TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);

/// Sends a report to the broker, which mails it on.
///
/// `kind` is what the operator chose it was -- a fault, a suggestion -- and
/// `email` may be empty: a church that does not want a reply should not have to
/// invent an address to tell us something.
pub fn send(
    endpoint: &str,
    app: &str,
    version: &str,
    kind: &str,
    text: &str,
    email: &str,
) -> Result<()> {
    if text.trim().is_empty() {
        bail!("There is nothing in the report to send.");
    }

    let body = serde_json::json!({
        "kind": kind,
        "app": app,
        "version": version,
        "platform": std::env::consts::OS,
        "email": email,
        "text": text,
    });

    let response = crate::tls::client()
        .timeout(TIMEOUT)
        .build()
        .context("could not prepare the client")?
        .post(endpoint)
        .json(&body)
        .send()
        .map_err(|error| anyhow::anyhow!("That could not be sent. {error}"))?;

    if response.status().is_success() {
        return Ok(());
    }

    // The broker words this for a church; passing it through beats inventing a
    // second sentence that says the same thing differently.
    let message = response
        .json::<serde_json::Value>()
        .ok()
        .and_then(|body| body.get("error").and_then(|e| e.as_str()).map(String::from))
        .unwrap_or_else(|| "That could not be sent just now.".into());
    bail!(message)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An empty report is refused here rather than posted and rejected.
    ///
    /// The round trip to find out is twenty seconds a church spends learning
    /// something the form already knew.
    #[test]
    fn refuses_an_empty_report() {
        assert!(send("http://127.0.0.1:1", "Castavox", "0.0.0", "fault", "   ", "").is_err());
    }
}
