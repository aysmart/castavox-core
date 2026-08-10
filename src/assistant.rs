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

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

/// Azure AI Foundry's inference API is versioned by query string.
pub const DEFAULT_API_VERSION: &str = "2024-05-01-preview";

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
pub fn is_response_format_refusal(payload: &str) -> bool {
    let lower = payload.to_lowercase();
    lower.contains("response_format")
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
        Err(error) if strict_json && is_response_format_refusal(&error.to_string()) => {
            // The deployment does not take the parameter. Ask again without it
            // and read the answer more forgivingly; that is the caller's job.
            let mut plain = body;
            plain.as_object_mut().map(|map| map.remove("response_format"));
            content_of(&send(client, route, &plain)?)
        }
        Err(error) => Err(error),
    }
}

fn send(
    client: &reqwest::blocking::Client,
    route: &Route<'_>,
    body: &serde_json::Value,
) -> Result<String> {
    crate::tls::install();

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
        .context("could not reach the model")?;

    let status = response.status();
    let payload = response.text().unwrap_or_default();

    if status.is_redirection() {
        // The same trap as the broker: a redirect that changes host drops the
        // credential, and what comes back blames the caller for it.
        bail!("the model endpoint redirected; it must name the canonical host");
    }
    if !status.is_success() {
        bail!("the model returned {status}: {}", payload.chars().take(300).collect::<String>());
    }
    Ok(payload)
}

/// Digs the message content out of a chat completion.
fn content_of(payload: &str) -> Result<String> {
    let parsed: serde_json::Value =
        serde_json::from_str(payload).context("the model response was not JSON")?;

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
