//! Talking to a language model, on whoever's account.
//!
//! Both products ask a model two things: to suggest which scripture a speaker
//! is paraphrasing, and to summarise a service afterwards. What they ask is
//! their own business — the prompts, the verification against the local
//! library, none of that is here. What is here is the part that was written
//! twice and has to be identical: where the request goes, how it is
//! authenticated, and what to make of the answer.
//!
//! # Two accounts
//!
//! A church can point at its own Azure AI Foundry deployment with its own key,
//! which is how this has always worked and stays free of us entirely. Or it can
//! be on a subscription, in which case the request goes to our broker, which
//! holds the key and meters the call. [`Route`] is that choice, and it is the
//! only thing either caller has to know about the difference.
//!
//! Our key is never in either product. On a subscription the app carries a
//! device token, and the model credentials stay on the server.

use serde::{Deserialize, Serialize};

/// Azure AI Foundry's inference API is versioned by query string.
pub const DEFAULT_API_VERSION: &str = "2024-05-01-preview";

/// Why a completion did not come back.
///
/// Typed rather than a message, because the caller has to word the advice and
/// the advice depends on the status. A 404 from Foundry almost never means the
/// deployment is missing — it means the API version predates the `/models`
/// route — and telling somebody their deployment is gone sends them to rebuild
/// something that is fine. Only the caller knows whether the operator has a key
/// to check or is on a subscription and has nothing to check at all.
///
/// The payload is carried rather than summarised. It goes to the log, never to
/// the screen: what comes back can be a page of somebody else's HTML, and
/// putting that in front of an operator buries the one sentence that would have
/// helped.
#[derive(Debug)]
pub enum Failure {
    /// Network, TLS, a timeout. Nothing a different body would fix.
    Unreachable(String),
    /// It answered, and said no.
    Refused {
        status: reqwest::StatusCode,
        payload: String,
    },
    /// It answered with something that was not a completion.
    Unexpected(String),
}

/// A request failure, with the chain reqwest hides behind one sentence.
///
/// `reqwest::Error` prints "error sending request for url (…)" and stops. The
/// cause -- a refused connection, a TLS handshake, a resolver, a runtime that
/// would not let a blocking client run -- is one level down and is the only
/// part anybody can act on. Reporting the summary alone cost an afternoon: a
/// machine where `curl` answered in two seconds, an application that could not
/// send at all, and a log line that said neither why.
fn because(error: &reqwest::Error) -> String {
    let mut said = error.to_string();
    let mut source = std::error::Error::source(error);
    // Capped: a chain is usually two or three deep, and a log line is not a
    // place to print an unbounded one.
    for _ in 0..5 {
        let Some(cause) = source else { break };
        said.push_str(&format!(" — {cause}"));
        source = cause.source();
    }
    said
}

impl std::fmt::Display for Failure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Failure::Unreachable(detail) => write!(formatter, "could not reach the model: {detail}"),
            Failure::Refused { status, payload } => write!(
                formatter,
                "the model returned {status}: {}",
                payload.chars().take(300).collect::<String>()
            ),
            Failure::Unexpected(detail) => write!(formatter, "{detail}"),
        }
    }
}

impl std::error::Error for Failure {}

pub type Result<T> = std::result::Result<T, Failure>;

/// A church's own deployment.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Deployment {
    /// e.g. https://my-resource.services.ai.azure.com/models
    pub endpoint: String,
    /// Deployment or model name, e.g. gpt-4o-mini or Llama-3.3-70B-Instruct.
    pub model: String,
    pub api_version: String,
}

/// Where a completion is sent, and on whose account.
pub enum Route<'a> {
    /// The church's own deployment and key. Costs them nothing through us and
    /// reaches nothing of ours.
    Own {
        deployment: &'a Deployment,
        key: &'a str,
    },
    /// Our deployment, by way of the broker, metered against a subscription.
    Hosted {
        /// The broker's canonical host. A redirect would cost us the
        /// credential — see `hosted::DEFAULT_BASE`.
        base: &'a str,
        device_token: &'a str,
    },
}

impl Route<'_> {
    /// Whether this route has enough to be worth trying.
    pub fn usable(&self) -> bool {
        match self {
            Route::Own { deployment, key } => {
                !deployment.endpoint.trim().is_empty()
                    && !deployment.model.trim().is_empty()
                    && !key.trim().is_empty()
            }
            Route::Hosted { base, device_token } => {
                !base.trim().is_empty() && !device_token.trim().is_empty()
            }
        }
    }
}

/// Azure AI Foundry's inference route, tolerating either the resource root or a
/// full path, with or without a trailing slash.
///
/// # Why the `/models` segment is added
///
/// The portal shows the resource as `https://name.services.ai.azure.com/`,
/// which is what an operator naturally pastes in — and posting straight to that
/// answers 404, because the resource is a front door for several inference APIs
/// rather than one of them. Adding the segment here means the address people are
/// actually given is the address that works.
///
/// It cost a while to find, because a 404 from a missing path segment looks
/// exactly like a 404 from a typo.
///
/// Left alone when they have already named a route: `/models` because they read
/// the documentation, `/openai/deployments/...` because they are pointing at an
/// Azure OpenAI deployment, which is a different shape entirely.
pub fn endpoint_url(endpoint: &str, api_version: &str) -> String {
    let base = endpoint.trim().trim_end_matches('/');

    let foundry = base
        .split("://")
        .nth(1)
        .and_then(|rest| rest.split('/').next())
        .is_some_and(|host| host.ends_with(".services.ai.azure.com"));
    let routed = base.contains("/models") || base.contains("/openai/");

    let base = if foundry && !routed { format!("{base}/models") } else { base.to_string() };

    let path = if base.ends_with("/chat/completions") {
        base.clone()
    } else {
        format!("{base}/chat/completions")
    };
    let version = if api_version.trim().is_empty() {
        DEFAULT_API_VERSION
    } else {
        api_version.trim()
    };
    format!("{path}?api-version={version}")
}

/// Whether a refusal was about `response_format` rather than about us.
///
/// A deployment backed by OpenAI honours `response_format` and returns JSON
/// that always parses; a Llama or Mistral deployment rejects the parameter
/// outright. Neither is wrong, and an operator cannot be expected to know which
/// family their deployment belongs to, so the strict request is tried and this
/// decides whether the refusal is worth retrying without it.
///
/// Matched on the message because there is no code for it: the families that
/// cannot do this disagree about the status, the error type and the wording,
/// and all that is reliably shared is that they name the thing they are
/// complaining about. A false positive costs one retry, which generates no
/// tokens; a false negative costs the operator their summary, so this errs
/// towards retrying.
///
/// All three spellings, because both summarisers carried this list and this
/// only carried the first — so a deployment that said "json_object" got the
/// retry in one product and a failed summary in the other.
pub fn is_response_format_refusal(payload: &str) -> bool {
    let lower = payload.to_lowercase();
    lower.contains("response_format") || lower.contains("json_object") || lower.contains("json mode")
}

/// Whether a refusal is the model saying the transcript was too long.
///
/// This is the difference between "the model could not be reached" and "the
/// model was reached, read the whole request, and would not accept it" -- and
/// telling a church the first when the second happened sends them to check a
/// network that was never at fault. A four-hour teaching session produced a
/// 27,000-word transcript, the deployment refused it on length, and the
/// operator was advised to try again in a moment. It would have failed the same
/// way every time.
///
/// Matched on the wording for the same reason as [`is_response_format_refusal`]:
/// the families disagree on the code and the status. OpenAI and Azure send
/// `context_length_exceeded`; Anthropic says the prompt is too long; others
/// only ever mention the maximum context length. What they share is that they
/// name it.
pub fn is_too_long_refusal(payload: &str) -> bool {
    let lower = payload.to_lowercase();
    lower.contains("context_length_exceeded")
        || lower.contains("string_above_max_length")
        || lower.contains("maximum context length")
        || lower.contains("context length")
        || lower.contains("prompt is too long")
        || lower.contains("too many tokens")
        || lower.contains("reduce the length")
        || lower.contains("max_tokens exceed")
}

/// A long transcript cut into overlapping stretches, each small enough to send.
///
/// `size` and `overlap` are in words, because words are what a transcript is
/// counted in and what an operator can reason about. Tokens are the thing the
/// model actually counts, but the ratio depends on the model, so a word budget
/// with room to spare beats a token budget that is wrong per deployment.
///
/// # Why they overlap
///
/// A teaching session does not divide at a word boundary. Cut it cleanly at
/// word 12,000 and whatever was being argued across that line is halved: the
/// passage is read at the end of one stretch and expounded at the start of the
/// next, and neither summary has both. The overlap gives each stretch the tail
/// of the one before, so a point that straddles a cut is whole in at least one
/// of them. A little is repeated, and repetition is what the synthesis is for.
///
/// Returns one stretch -- the whole text -- when it already fits.
pub fn chunks(text: &str, size: usize, overlap: usize) -> Vec<String> {
    let words: Vec<&str> = text.split_whitespace().collect();
    if words.is_empty() {
        return Vec::new();
    }
    if words.len() <= size {
        return vec![words.join(" ")];
    }

    // A stretch has to advance, or this never ends.
    let step = size.saturating_sub(overlap).max(1);

    let mut out = Vec::new();
    let mut start = 0;
    while start < words.len() {
        let end = (start + size).min(words.len());
        out.push(words[start..end].join(" "));
        if end == words.len() {
            break;
        }
        start += step;
    }
    out
}

/// Sends a chat completion and returns the message content.
///
/// `body` is the request as the caller wants it — model, messages, whatever
/// else. The model name is filled in for the church's own deployment and left
/// to the broker for a hosted one, which knows what it is running.
///
/// `strict_json` asks for a JSON object and retries without that parameter if
/// the deployment rejects it, because the two model families disagree and the
/// operator should not have to know which they bought.
pub fn complete(
    client: &reqwest::blocking::Client,
    route: &Route<'_>,
    mut body: serde_json::Value,
    strict_json: bool,
) -> Result<String> {
    if strict_json {
        body["response_format"] = serde_json::json!({ "type": "json_object" });
    }

    match send(client, route, &body) {
        Ok(payload) => content_of(&payload),
        // Matched on the payload rather than on a rendered message. The
        // rendered one truncates at 300 characters, so a deployment that named
        // the parameter later than that in its complaint never got the retry --
        // it simply failed, and looked like a deployment that was down.
        Err(Failure::Refused { payload, .. }) if strict_json && is_response_format_refusal(&payload) => {
            // The deployment does not take the parameter. Ask again without it
            // and read the answer more forgivingly; that is the caller's job.
            let mut plain = body;
            plain.as_object_mut().map(|map| map.remove("response_format"));
            content_of(&send(client, route, &plain)?)
        }
        Err(failure) => Err(failure),
    }
}

fn send(
    client: &reqwest::blocking::Client,
    route: &Route<'_>,
    body: &serde_json::Value,
) -> Result<String> {

    let request = match route {
        Route::Own { deployment, key } => client
            .post(endpoint_url(&deployment.endpoint, &deployment.api_version))
            .header("api-key", *key)
            // Bearer as well as api-key: Foundry's model router accepts the
            // former and some deployments only answer to one of the two.
            .bearer_auth(*key),
        Route::Hosted { base, device_token } => client
            .post(format!("{}/api/v1/assist", base.trim_end_matches('/')))
            .bearer_auth(*device_token),
    };

    let response = request
        .header("content-type", "application/json")
        .json(body)
        .send()
        .map_err(|error| Failure::Unreachable(because(&error)))?;

    let status = response.status();
    let payload = response.text().unwrap_or_default();

    if status.is_redirection() {
        // The same trap as the broker: a redirect that changes host drops the
        // credential, and what comes back blames the caller for it.
        return Err(Failure::Unexpected(
            "the model endpoint redirected; it must name the canonical host".to_string(),
        ));
    }
    if !status.is_success() {
        return Err(Failure::Refused { status, payload });
    }
    Ok(payload)
}

/// Digs the message content out of a chat completion.
fn content_of(payload: &str) -> Result<String> {
    let parsed: serde_json::Value = serde_json::from_str(payload).map_err(|error| {
        Failure::Unexpected(format!("the model response was not JSON: {error}"))
    })?;

    Ok(parsed
        .get("choices")
        .and_then(|choices| choices.get(0))
        .and_then(|choice| choice.get("message"))
        .and_then(|message| message.get("content"))
        .and_then(|content| content.as_str())
        .unwrap_or_default()
        .to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_text_that_fits_is_one_stretch() {
        let text = "a b c d e";
        assert_eq!(chunks(text, 10, 2), vec!["a b c d e".to_string()]);
    }

    #[test]
    fn stretches_overlap_so_nothing_straddles_a_cut_unseen() {
        let words: Vec<String> = (0..10).map(|i| format!("w{i}")).collect();
        let out = chunks(&words.join(" "), 4, 1);
        // Steps of 3, so: 0-3, 3-6, 6-9, 9.
        assert_eq!(out[0], "w0 w1 w2 w3");
        assert_eq!(out[1], "w3 w4 w5 w6");
        assert!(out.last().unwrap().ends_with("w9"), "the end is never dropped");
    }

    #[test]
    fn every_word_survives_the_cutting() {
        let words: Vec<String> = (0..1_000).map(|i| format!("w{i}")).collect();
        let joined = words.join(" ");
        let out = chunks(&joined, 130, 20);
        for word in &words {
            assert!(
                out.iter().any(|c| c.split_whitespace().any(|w| w == word)),
                "{word} was lost between stretches"
            );
        }
    }

    #[test]
    fn an_overlap_as_large_as_the_stretch_still_terminates() {
        let words: Vec<String> = (0..50).map(|i| format!("w{i}")).collect();
        let out = chunks(&words.join(" "), 10, 10);
        assert!(out.len() <= 50, "made no progress and looped");
        assert!(out.last().unwrap().ends_with("w49"));
    }

    #[test]
    fn recognises_a_refusal_on_length() {
        for payload in [
            r#"{"error":{"code":"context_length_exceeded","message":"This model's maximum context length is 8192 tokens"}}"#,
            r#"{"error":{"type":"invalid_request_error","message":"prompt is too long: 214000 tokens > 200000"}}"#,
            r#"{"error":{"message":"Please reduce the length of the messages."}}"#,
        ] {
            assert!(is_too_long_refusal(payload), "not recognised: {payload}");
        }
    }

    #[test]
    fn does_not_mistake_other_refusals_for_length() {
        for payload in [
            r#"{"error":{"code":"401","message":"Access denied due to invalid subscription key"}}"#,
            r#"{"error":{"message":"response_format is not supported"}}"#,
        ] {
            assert!(!is_too_long_refusal(payload), "wrongly recognised: {payload}");
        }
    }

    #[test]
    fn a_refusal_about_json_mode_is_told_from_a_real_failure() {
        // The families that cannot do this each phrase it differently, and the
        // point of matching loosely is that a new one should still be caught.
        assert!(is_response_format_refusal(
            r#"{"error":{"message":"Extra inputs are not permitted: response_format"}}"#
        ));
        assert!(is_response_format_refusal(
            r#"{"error":{"message":"'json_object' is not supported by this model"}}"#
        ));
        assert!(is_response_format_refusal(
            r#"{"error":{"message":"JSON mode is unavailable"}}"#
        ));

        // These must not trigger a retry: the second attempt would fail the
        // same way and the operator would wait twice as long to be told.
        assert!(!is_response_format_refusal(
            r#"{"error":{"code":"401","message":"Access denied"}}"#
        ));
        assert!(!is_response_format_refusal(
            r#"{"error":{"code":"404","message":"Resource not found"}}"#
        ));
        assert!(!is_response_format_refusal(
            r#"{"error":{"message":"Rate limit exceeded"}}"#
        ));
    }

    #[test]
    fn a_refusal_names_its_status_so_the_caller_can_word_the_advice() {
        // The reason Failure is typed rather than a string. A 404 from Foundry
        // almost never means the deployment is missing, and only the caller
        // knows whether the operator has a key to check or is on a
        // subscription and has nothing to check at all.
        let failure = Failure::Refused {
            status: reqwest::StatusCode::NOT_FOUND,
            payload: "<html>Resource not found</html>".to_string(),
        };
        let Failure::Refused { status, .. } = &failure else { panic!("wrong kind") };
        assert_eq!(status.as_u16(), 404);

        // And the payload is carried whole, for the log, rather than being
        // summarised into the message an operator would see.
        assert!(failure.to_string().contains("404"));
    }

    #[test]
    fn a_foundry_resource_root_gains_the_models_segment() {
        // What the portal shows and an operator pastes. Without the segment it
        // is a 404 that looks exactly like a typo.
        assert_eq!(
            endpoint_url("https://church-gpt.services.ai.azure.com", "2024-05-01-preview"),
            "https://church-gpt.services.ai.azure.com/models/chat/completions?api-version=2024-05-01-preview"
        );
        // Trailing slash, same answer.
        assert_eq!(
            endpoint_url("https://church-gpt.services.ai.azure.com/", "v1"),
            "https://church-gpt.services.ai.azure.com/models/chat/completions?api-version=v1"
        );
    }

    #[test]
    fn a_route_the_operator_already_named_is_left_alone() {
        assert!(endpoint_url("https://x.services.ai.azure.com/models", "v1")
            .starts_with("https://x.services.ai.azure.com/models/chat/completions"));
        // Azure OpenAI is a different shape and must not be rewritten.
        let openai = endpoint_url("https://x.openai.azure.com/openai/deployments/gpt/", "v1");
        assert!(!openai.contains("/models"), "{openai}");
    }

    #[test]
    fn a_host_that_is_not_foundry_is_never_rewritten() {
        let other = endpoint_url("https://api.example.test/v1", "2024-05-01-preview");
        assert_eq!(other, "https://api.example.test/v1/chat/completions?api-version=2024-05-01-preview");
    }

    #[test]
    fn an_empty_version_falls_back_rather_than_sending_nothing() {
        assert!(endpoint_url("https://api.example.test", "   ")
            .ends_with(&format!("api-version={DEFAULT_API_VERSION}")));
    }

    #[test]
    fn a_refusal_about_response_format_is_told_from_a_real_one() {
        // What a Llama deployment actually answers.
        assert!(is_response_format_refusal(
            r#"{"error":{"message":"Extra inputs are not permitted: response_format"}}"#
        ));
        // And what a genuine problem looks like, which must not be retried.
        assert!(!is_response_format_refusal(
            r#"{"error":{"message":"Access denied due to invalid subscription key"}}"#
        ));
    }

    #[test]
    fn a_route_knows_when_it_has_nothing_to_offer() {
        let empty = Deployment::default();
        assert!(!Route::Own { deployment: &empty, key: "k" }.usable());

        let full = Deployment {
            endpoint: "https://x.services.ai.azure.com".into(),
            model: "gpt-4o-mini".into(),
            api_version: String::new(),
        };
        assert!(Route::Own { deployment: &full, key: "k" }.usable());
        // A deployment without a key is a deployment nobody can call.
        assert!(!Route::Own { deployment: &full, key: "  " }.usable());

        assert!(Route::Hosted { base: "https://www.castavox.com", device_token: "t" }.usable());
        assert!(!Route::Hosted { base: "https://www.castavox.com", device_token: "" }.usable());
    }
}
