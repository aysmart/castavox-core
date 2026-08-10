//! A hosted subscription, from the app's side.
//!
//! Activation exchanges a licence key for a device token. Entitlement reports
//! what is left of the plan, so the operator can see the hours without starting
//! a service to find out.
//!
//! The speech session is deliberately not here. It lives in the Node bridge,
//! whose process lifetime *is* the session — see `bridge/index.js`. Splitting it
//! would mean two processes with opinions about what is open, and the one that
//! is wrong leaves a session running.
//!
//! # What a machine holds
//!
//! A device token, and nothing else. Our Azure key is on the server and reaches
//! neither product. The token buys ten-minute Azure credentials metered against
//! a plan, and the broker can revoke it. It is a secret, so it belongs in the
//! platform credential store and never in a settings file — the products own
//! that part, because each already has its own keychain service name.

use std::collections::BTreeMap;
use std::time::Duration;

use serde::{Deserialize, Serialize};

/// Where the broker lives, unless a build or a test says otherwise.
const DEFAULT_BASE: &str = "https://castavox.com";

/// Long enough for a slow connection, short enough that a settings dialog is
/// not simply stuck.
const TIMEOUT: Duration = Duration::from_secs(20);

/// Which product is asking. The broker meters the two separately.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum App {
    Castavox,
    Pulpitry,
}

impl App {
    pub fn as_str(self) -> &'static str {
        match self {
            App::Castavox => "castavox",
            App::Pulpitry => "pulpitry",
        }
    }
}

/// What went wrong, in the only three ways that lead anywhere different.
#[derive(Debug)]
pub enum Error {
    /// The broker answered and said no. The message was written for the person
    /// at the desk and is shown to them unchanged.
    Refused { reason: String, message: String },
    /// The broker could not be reached at all.
    Unreachable(String),
    /// It answered something we could not read, which is our problem and not
    /// something to explain to a church in detail.
    Unexpected(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Refused { message, .. } => write!(f, "{message}"),
            Error::Unreachable(_) => write!(
                f,
                "Could not reach Castavox. Check the internet connection and try again."
            ),
            Error::Unexpected(_) => write!(
                f,
                "Castavox gave an answer this version did not understand. Try updating."
            ),
        }
    }
}

impl std::error::Error for Error {}

impl Error {
    /// The machine-readable reason, for code that must tell refusals apart.
    pub fn reason(&self) -> &str {
        match self {
            Error::Refused { reason, .. } => reason,
            Error::Unreachable(_) => "unreachable",
            Error::Unexpected(_) => "unexpected",
        }
    }

    /// Whether trying the same thing later might work.
    pub fn is_temporary(&self) -> bool {
        matches!(self, Error::Unreachable(_))
    }
}

pub type Result<T> = std::result::Result<T, Error>;

/// What a plan has left this period.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Remaining {
    pub speech_seconds: i64,
    pub assistant_requests: i64,
    /// 0 to 1, of the allowance rather than of the ceiling.
    pub speech_used_fraction: f64,
    pub assistant_used_fraction: f64,
    /// The highest warning threshold passed, or none while there is room.
    pub warn_at: Option<f64>,
    pub period_ends_at: i64,
}

/// What one product has spent, so an operator can see where the hours went.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AppUsage {
    pub speech_seconds: i64,
    pub assistant_requests: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Usage {
    pub total: i64,
    pub by_app: BTreeMap<String, AppUsage>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Entitlement {
    pub status: String,
    pub expires_at: Option<i64>,
    pub period_ends_at: i64,
    pub remaining: Remaining,
    pub usage: Usage,
}

impl Entitlement {
    /// Whether this subscription may be used at all.
    pub fn is_active(&self) -> bool {
        self.status == "active"
    }
}

/// The answer to activating a machine. The token is returned once, here.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Activated {
    pub token: String,
    pub subscription_id: String,
}

/// A stable identifier for this machine, made once and kept.
///
/// Random rather than derived from the hardware: a machine fingerprint is a
/// thing to have to explain to a customer, and the only job here is to let one
/// laptop activate twice without consuming two of the seats they paid for.
pub fn new_device_id() -> String {
    let mut bytes = [0u8; 16];
    // A failure here would mean the OS has no entropy, which is not a case with
    // a sensible fallback; the time is at least unique enough to activate with.
    if getrandom::fill(&mut bytes).is_err() {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        bytes[..16].copy_from_slice(&nanos.to_le_bytes());
    }
    let mut id = String::from("dev_");
    for byte in bytes {
        id.push_str(&format!("{byte:02x}"));
    }
    id
}

/// Hours and minutes, the way both products say it.
///
/// Here rather than in each product so that a church reading Castavox and
/// Pulpitry side by side is not told two different things about one number.
pub fn readable(seconds: i64) -> String {
    let seconds = seconds.max(0);
    let hours = seconds / 3600;
    let minutes = (seconds % 3600) / 60;
    match (hours, minutes) {
        (0, 0) => "none left".to_string(),
        (0, m) => format!("{m} min"),
        (h, 0) => format!("{h} hr"),
        (h, m) => format!("{h} hr {m} min"),
    }
}

/// The broker, as the app talks to it.
pub struct Broker {
    base: String,
}

impl Broker {
    /// Points at the live broker, or wherever `CASTAVOX_BROKER_URL` says.
    ///
    /// The variable exists so a staging broker can be pointed at without a
    /// build, and so the tests can run against a local one.
    pub fn new() -> Self {
        let base = std::env::var("CASTAVOX_BROKER_URL")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| DEFAULT_BASE.to_string());
        Self::at(&base)
    }

    pub fn at(base: &str) -> Self {
        Self {
            base: base.trim_end_matches('/').to_string(),
        }
    }

    /// What the bridge should be told, so it can open its own session.
    pub fn base_url(&self) -> &str {
        &self.base
    }

    fn client(&self) -> Result<reqwest::blocking::Client> {
        // Before the client is built, not after. Installing the provider
        // afterwards leaves the client on whatever was already there, which
        // fails for every user rather than for none of them.
        crate::tls::install();
        reqwest::blocking::Client::builder()
            .timeout(TIMEOUT)
            .build()
            .map_err(|error| Error::Unexpected(error.to_string()))
    }

    /// Exchanges a licence key for a device token.
    ///
    /// Accepted once per machine. Everything afterwards presents the token, so
    /// the short guessable string is exposed as rarely as it can be.
    pub fn activate(
        &self,
        licence: &str,
        device_id: &str,
        app: App,
        name: &str,
    ) -> Result<Activated> {
        let response = self
            .client()?
            .post(format!("{}/api/v1/activate", self.base))
            .json(&serde_json::json!({
                "licence": licence.trim(),
                "deviceId": device_id,
                "app": app.as_str(),
                "name": name,
            }))
            .send()
            .map_err(|error| Error::Unreachable(error.to_string()))?;

        read(response)
    }

    /// What this subscription may do, and what is left of it.
    pub fn entitlement(&self, device_token: &str) -> Result<Entitlement> {
        let response = self
            .client()?
            .get(format!("{}/api/v1/entitlement", self.base))
            .bearer_auth(device_token)
            .send()
            .map_err(|error| Error::Unreachable(error.to_string()))?;

        read(response)
    }
}

impl Default for Broker {
    fn default() -> Self {
        Self::new()
    }
}

/// Turns a response into either the thing asked for or a refusal worth showing.
fn read<T: serde::de::DeserializeOwned>(response: reqwest::blocking::Response) -> Result<T> {
    let status = response.status();
    let body = response
        .text()
        .map_err(|error| Error::Unreachable(error.to_string()))?;

    if status.is_success() {
        return serde_json::from_str(&body).map_err(|error| Error::Unexpected(error.to_string()));
    }

    // The broker words its refusals for the person at the desk. Anything else
    // is a shape we did not expect, and inventing a message for it would be
    // worse than admitting that.
    #[derive(Deserialize)]
    struct Refusal {
        error: Option<String>,
        message: Option<String>,
    }

    match serde_json::from_str::<Refusal>(&body) {
        Ok(refusal) if refusal.message.is_some() || refusal.error.is_some() => Error::Refused {
            reason: refusal.error.unwrap_or_else(|| "refused".to_string()),
            message: refusal
                .message
                .unwrap_or_else(|| "Castavox refused that request.".to_string()),
        },
        _ => Error::Unexpected(format!("HTTP {status}")),
    }
    .into_err()
}

/// Small helper so the match above reads as one expression.
trait IntoErr {
    fn into_err<T>(self) -> Result<T>;
}

impl IntoErr for Error {
    fn into_err<T>(self) -> Result<T> {
        Err(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;
    use std::sync::mpsc;
    use std::thread;

    /// A one-shot HTTP server that answers whatever the test asks it to.
    ///
    /// Hand-rolled rather than pulled in: what is being checked is how this
    /// module reads a status and a body, and a real server would only add a
    /// dependency between the test and something else's idea of a route.
    fn serve(status: u16, body: &'static str) -> (String, mpsc::Receiver<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("a port");
        let base = format!("http://{}", listener.local_addr().unwrap());
        let (sender, receiver) = mpsc::channel();

        thread::spawn(move || {
            let Ok((stream, _)) = listener.accept() else {
                return;
            };
            let mut reader = BufReader::new(stream);

            // Enough of the request to assert on: the line, the headers, and a
            // body if one was announced.
            let mut request = String::new();
            let mut length = 0usize;
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 {
                    break;
                }
                if let Some(value) = line.to_lowercase().strip_prefix("content-length:") {
                    length = value.trim().parse().unwrap_or(0);
                }
                let done = line.trim().is_empty();
                request.push_str(&line);
                if done {
                    break;
                }
            }
            if length > 0 {
                let mut payload = vec![0u8; length];
                let _ = reader.read_exact(&mut payload);
                request.push_str(&String::from_utf8_lossy(&payload));
            }
            let _ = sender.send(request);

            let reason = if status < 300 { "OK" } else { "NO" };
            let response = format!(
                "HTTP/1.1 {status} {reason}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = reader.get_mut().write_all(response.as_bytes());
            let _ = reader.get_mut().flush();
        });

        (base, receiver)
    }

    #[test]
    fn activation_sends_the_licence_and_returns_the_token() {
        let (base, requests) =
            serve(200, r#"{"token":"dev-token-abc","subscriptionId":"sub_1"}"#);

        let activated = Broker::at(&base)
            .activate("aaaa-bbbb-cccc-dddd", "dev_123", App::Pulpitry, "Desk")
            .expect("activation");

        assert_eq!(activated.token, "dev-token-abc");
        assert_eq!(activated.subscription_id, "sub_1");

        let request = requests.recv().expect("the request");
        assert!(request.contains("POST /api/v1/activate"));
        // The app is sent, because the broker meters the two separately.
        assert!(request.contains(r#""app":"pulpitry""#));
        assert!(request.contains(r#""deviceId":"dev_123""#));
    }

    #[test]
    fn a_refusal_is_shown_to_the_operator_word_for_word() {
        let (base, _requests) = serve(
            404,
            r#"{"error":"unknown_licence","message":"That licence key was not recognised. Check it against your invoice."}"#,
        );

        let error = Broker::at(&base)
            .activate("nope", "dev_123", App::Castavox, "")
            .expect_err("a refusal");

        assert_eq!(error.reason(), "unknown_licence");
        // Unchanged: the broker wrote it for the person reading it.
        assert_eq!(
            error.to_string(),
            "That licence key was not recognised. Check it against your invoice."
        );
        assert!(!error.is_temporary());
    }

    #[test]
    fn entitlement_reads_what_is_left_and_where_it_went() {
        let (base, requests) = serve(
            200,
            r#"{"status":"active","expiresAt":1800000000000,"periodEndsAt":1790000000000,
                "remaining":{"speechSeconds":45000,"assistantRequests":80,"speechUsedFraction":0.58,
                             "assistantUsedFraction":0.2,"warnAt":null,"periodEndsAt":1790000000000},
                "usage":{"total":63000,"byApp":{"pulpitry":{"speechSeconds":40000,"assistantRequests":10},
                                                "castavox":{"speechSeconds":23000,"assistantRequests":10}}}}"#,
        );

        let state = Broker::at(&base).entitlement("dev-token-abc").expect("state");

        assert!(state.is_active());
        assert_eq!(state.remaining.speech_seconds, 45000);
        assert_eq!(state.usage.total, 63000);
        // The per-app split the pooled quota hides, which is the requirement.
        assert_eq!(state.usage.by_app["pulpitry"].speech_seconds, 40000);
        assert_eq!(state.usage.by_app["castavox"].speech_seconds, 23000);

        let request = requests.recv().expect("the request");
        assert!(request.contains("GET /api/v1/entitlement"));
        assert!(request.to_lowercase().contains("authorization: bearer dev-token-abc"));
    }

    #[test]
    fn a_broker_that_cannot_be_reached_is_worth_trying_again() {
        // A port with nothing behind it.
        let error = Broker::at("http://127.0.0.1:1")
            .entitlement("dev-token-abc")
            .expect_err("unreachable");

        assert!(error.is_temporary());
        assert!(error.to_string().contains("internet connection"));
    }

    #[test]
    fn an_answer_we_cannot_read_does_not_become_advice() {
        let (base, _requests) = serve(500, "<html>gateway</html>");

        let error = Broker::at(&base)
            .entitlement("dev-token-abc")
            .expect_err("unexpected");

        assert_eq!(error.reason(), "unexpected");
        // Not dressed up as a subscription problem, which it is not.
        assert!(error.to_string().contains("did not understand"));
    }

    #[test]
    fn tolerates_fields_this_version_has_not_heard_of() {
        // The broker will grow fields. An older app must keep working rather
        // than refuse to read its own subscription.
        let (base, _requests) = serve(
            200,
            r#"{"status":"active","periodEndsAt":1,"somethingNew":{"x":1},
                "remaining":{"speechSeconds":10,"newThing":true},"usage":{"total":5}}"#,
        );

        let state = Broker::at(&base).entitlement("t").expect("state");
        assert_eq!(state.remaining.speech_seconds, 10);
        assert_eq!(state.usage.total, 5);
        assert!(state.usage.by_app.is_empty());
    }

    #[test]
    fn device_ids_are_distinct() {
        let a = new_device_id();
        let b = new_device_id();
        assert_ne!(a, b);
        assert!(a.starts_with("dev_"));
        assert_eq!(a.len(), 4 + 32);
    }

    #[test]
    fn hours_read_the_way_somebody_would_say_them() {
        assert_eq!(readable(0), "none left");
        assert_eq!(readable(59), "none left");
        assert_eq!(readable(90), "1 min");
        assert_eq!(readable(3600), "1 hr");
        assert_eq!(readable(3660), "1 hr 1 min");
        assert_eq!(readable(45_000), "12 hr 30 min");
        // Never a negative, whatever arithmetic produced it.
        assert_eq!(readable(-5), "none left");
    }
}
