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

/**
 * How a service expects to be asked.
 *
 * Underneath, every one of them takes a list of messages and returns a message.
 * What differs is the address, the header the key goes in, and — for one of
 * them — where the system prompt lives. Three shapes cover everything a church
 * is likely to have.
 */
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Shape {
    /**
     * `POST {base}/chat/completions`, key as a bearer token.
     *
     * The default because most of the industry copied OpenAI's: it reaches
     * OpenAI itself, DeepSeek, Grok, Mistral, Groq, OpenRouter, Perplexity,
     * Gemini through its compatibility endpoint, and both of the runners a
     * church would use on its own machine — Ollama and LM Studio.
     */
    #[default]
    OpenAi,
    /// The same body at `?api-version=`, key in an `api-key` header. Azure
    /// OpenAI and AI Foundry.
    Azure,
    /**
     * `POST {base}/v1/messages`, and three differences that are each silent
     * when wrong.
     *
     * The system prompt is its own field rather than a message with
     * `role: system` — put it in the array and it is ignored. `max_tokens` is
     * required rather than optional — leave it out and the answer is a 400.
     * And the reply is `content[0].text` rather than
     * `choices[0].message.content` — read the wrong one and a good answer looks
     * like a model with nothing to say.
     */
    Anthropic,
}

/// A church's own deployment.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Deployment {
    /// e.g. `https://api.openai.com/v1`, or `http://localhost:11434/v1` for a
    /// model running on the machine itself.
    pub endpoint: String,
    /// Deployment or model name, e.g. gpt-4o-mini, deepseek-chat, llama3.1.
    pub model: String,
    /// Azure only, and ignored by the other two.
    pub api_version: String,
    pub shape: Shape,
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
pub fn endpoint_url(endpoint: &str, api_version: &str, shape: Shape) -> String {
    let base = endpoint.trim().trim_end_matches('/');

    if shape == Shape::Anthropic {
        // Anthropic names its own version in a header rather than a query, and
        // its path is not a chat completion.
        return if base.ends_with("/messages") {
            base.to_string()
        } else if base.ends_with("/v1") {
            format!("{base}/messages")
        } else {
            format!("{base}/v1/messages")
        };
    }

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

    if shape != Shape::Azure {
        // Only Azure versions its API in the query string. Appending it
        // elsewhere is at best ignored and at worst a 404.
        return path;
    }

    let version = if api_version.trim().is_empty() {
        DEFAULT_API_VERSION
    } else {
        api_version.trim()
    };
    format!("{path}?api-version={version}")
}

/**
 * Whether an endpoint may be plain HTTP.
 *
 * Only for a model on this machine. Ollama answers on `http://localhost:11434`
 * and LM Studio on `http://localhost:1234`, and refusing those would refuse the
 * whole point of running a model locally.
 *
 * Anywhere else it is a key and a sermon in clear across a church network,
 * which is not a thing to allow because somebody typed it.
 */
pub fn transport_is_safe(endpoint: &str) -> bool {
    let endpoint = endpoint.trim();
    if endpoint.starts_with("https://") {
        return true;
    }
    let Some(rest) = endpoint.strip_prefix("http://") else { return false };
    let host = rest.split('/').next().unwrap_or_default();
    let host = host.split(':').next().unwrap_or_default();
    matches!(host, "localhost" | "127.0.0.1" | "[::1]" | "::1")
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
    lower.contains("response_format")
        || lower.contains("json_object")
        || lower.contains("json mode")
        // A deployment that takes `json_object` but not a schema says so about
        // the schema, and the retry without any shape is the same remedy.
        || lower.contains("json_schema")
        || lower.contains("structured output")
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
        // Our own broker's word for it. On a subscription the model's reply
        // never reaches the app -- it describes our deployment -- so the broker
        // classifies the refusal and sends back a reason instead. Without this
        // the hosted path could never tell a transcript that was too long from
        // the service being down, which is the whole fault this exists to fix.
        || lower.contains("too_long")
}

/// Whether the connection was intercepted rather than merely failing.
///
/// A school, office or hotel network may terminate HTTPS at an appliance and
/// re-sign it with a certificate of its own. If that certificate is not trusted
/// by the machine, every request fails at the handshake -- and the message it
/// deserves is not "check the internet connection", because the connection is
/// working perfectly and checking it will find nothing.
///
/// Seen in the field as an Infoblox appliance re-issuing our own domain:
/// `invalid peer certificate: UnknownIssuer`.
pub fn is_intercepted(detail: &str) -> bool {
    let lower = detail.to_lowercase();
    lower.contains("unknownissuer")
        || lower.contains("invalid peer certificate")
        || lower.contains("certificate verify failed")
        || lower.contains("self-signed certificate")
        || lower.contains("unable to get local issuer")
}

/// Whether the model's safety filter objected to the request.
///
/// It objects to the *transcript*, and the transcript is what a machine heard,
/// not what anybody said: "ended up dying" and "what a sorcerer" are what
/// recognition made of an ordinary sermon. A church meets this through no fault
/// of its own and no wording of theirs will avoid it.
///
/// Worth telling apart because the remedy is peculiar: the filter judges the
/// whole prompt at once, so the same words split across smaller requests are
/// usually accepted, and only the stretch that actually contains the offending
/// phrase is refused.
pub fn is_filtered_refusal(payload: &str) -> bool {
    let lower = payload.to_lowercase();
    lower.contains("content_filter")
        || lower.contains("responsibleaipolicy")
        || lower.contains("content management policy")
        // Our broker's word for it, which is all the app sees on a subscription.
        || lower.contains("\"filtered\"")
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
    /*
     * A caller may bring its own, and a schema beats asking nicely.
     *
     * `strict_json` means "any JSON will do"; a caller that has set
     * `response_format` itself has usually set a schema, which a model that
     * supports one cannot break. Overwriting that with `json_object` would
     * throw away the only enforcement available and leave the rules to prose.
     */
    if strict_json && body.get("response_format").is_none() {
        body["response_format"] = serde_json::json!({ "type": "json_object" });
    }
    let asked_for_a_shape = body.get("response_format").is_some();

    match send_patiently(client, route, &body) {
        Ok(payload) => content_of(&payload),
        // Matched on the payload rather than on a rendered message. The
        // rendered one truncates at 300 characters, so a deployment that named
        // the parameter later than that in its complaint never got the retry --
        // it simply failed, and looked like a deployment that was down.
        //
        // Whoever set it: a deployment that cannot take a schema is refused the
        // same way whether we asked for the shape or the caller did.
        Err(Failure::Refused { payload, .. })
            if asked_for_a_shape && is_response_format_refusal(&payload) =>
        {
            // The deployment does not take the parameter. Ask again without it
            // and read the answer more forgivingly; that is the caller's job.
            let mut plain = body;
            plain.as_object_mut().map(|map| map.remove("response_format"));
            content_of(&send_patiently(client, route, &plain)?)
        }
        Err(failure) => Err(failure),
    }
}

/// How long to wait before giving up on a busy model, and how many times.
///
/// Four attempts over about half a minute. A quota resets on a rolling minute,
/// so waiting minutes would be the wrong shape: either it clears inside this
/// window or the deployment is too small for the work being asked of it, and
/// the second is worth saying rather than waiting out.
const BUSY_WAITS: [u64; 3] = [3, 8, 20];

/// The same request, with room made for a model that is busy.
///
/// A long service is summarised in several requests, one per stretch and one
/// to draw them together, and they go out as fast as they can be answered.
/// That is enough to exhaust a deployment's per-minute quota: a 27,000-word
/// service read in three stretches got through every stretch and was refused
/// on the last request of the four, so all the work was done and none of it
/// could be used.
///
/// Waiting is the whole remedy. `Retry-After` is honoured when the service
/// sends one, because it knows better than any guess here.
fn send_patiently(
    client: &reqwest::blocking::Client,
    route: &Route<'_>,
    body: &serde_json::Value,
) -> Result<String> {
    for (attempt, wait) in BUSY_WAITS.iter().enumerate() {
        match send(client, route, body) {
            Err(Failure::Refused { status, payload }) if status.as_u16() == 429 => {
                let seconds = retry_after(&payload).unwrap_or(*wait);
                crate::log::line(&format!(
                    "[assistant] busy; waiting {seconds}s before attempt {} of {}",
                    attempt + 2,
                    BUSY_WAITS.len() + 1
                ));
                std::thread::sleep(std::time::Duration::from_secs(seconds.min(60)));
            }
            other => return other,
        }
    }
    send(client, route, body)
}

/// Seconds the service asked us to wait, if it named a number.
fn retry_after(payload: &str) -> Option<u64> {
    let value = serde_json::from_str::<serde_json::Value>(payload).ok()?;
    let seconds = value.get("retryAfter").or_else(|| value.get("retry_after"))?;
    seconds
        .as_u64()
        .or_else(|| seconds.as_str().and_then(|text| text.parse().ok()))
}

fn send(
    client: &reqwest::blocking::Client,
    route: &Route<'_>,
    body: &serde_json::Value,
) -> Result<String> {

    if let Route::Own { deployment, .. } = route {
        // Refused here rather than by declaring the deployment unusable, so the
        // operator is told why instead of finding the assistant quietly off.
        if !transport_is_safe(&deployment.endpoint) {
            return Err(Failure::Unexpected(
                "That address is not https. A model on this machine may use http://localhost; \
                 anywhere else the key and the sermon would cross the network in clear."
                    .into(),
            ));
        }
    }

    let request = match route {
        Route::Own { deployment, key } => {
            let url = endpoint_url(&deployment.endpoint, &deployment.api_version, deployment.shape);
            match deployment.shape {
                // Both headers on Azure: Foundry's model router accepts a
                // bearer token and some deployments answer only to one of them.
                Shape::Azure => client.post(url).header("api-key", *key).bearer_auth(*key),
                Shape::OpenAi => client.post(url).bearer_auth(*key),
                Shape::Anthropic => client
                    .post(url)
                    .header("x-api-key", *key)
                    // Required, and the reason a first attempt returns 400
                    // rather than an answer.
                    .header("anthropic-version", ANTHROPIC_VERSION),
            }
        }
        Route::Hosted { base, device_token } => client
            .post(format!("{}/api/v1/assist", base.trim_end_matches('/')))
            .bearer_auth(*device_token),
    };

    let response = request
        .header("content-type", "application/json")
        .json(&shaped(route, body))
        .send()
        .map_err(|error| Failure::Unreachable(because(&error)))?;

    let status = response.status();

    /*
     * Who answered, when the answer is a failure.
     *
     * A 502 can come from our broker, from the platform it runs on, or from a
     * proxy in front of both, and the three mean entirely different things --
     * a refusal we classified, a function that fell over, and a request that
     * never arrived. They are indistinguishable from the body alone, and an
     * afternoon went into guessing between them. The headers say plainly:
     * `cf-ray` is set by the proxy, `x-vercel-id` by the platform, and a
     * JSON content type means our own code composed the reply.
     */
    if !status.is_success() {
        let header = |name: &str| {
            response
                .headers()
                .get(name)
                .and_then(|value| value.to_str().ok())
                .unwrap_or("-")
                .to_string()
        };
        crate::log::line(&format!(
            "[assistant] {status} from server={} content-type={} cf-ray={} x-vercel-id={}",
            header("server"),
            header("content-type"),
            header("cf-ray"),
            header("x-vercel-id"),
        ));
    }

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

/// The version Anthropic requires in a header, and dates rather than numbers.
const ANTHROPIC_VERSION: &str = "2023-06-01";

/**
 * The same request, addressed the way this service expects.
 *
 * Everything above builds one body: a model, a list of messages, and whatever
 * else the caller wanted. Two of the three shapes take it as it is. Anthropic
 * needs the system message lifted out into its own field and a `max_tokens`
 * that it will not default for us — and being wrong about either is silent
 * rather than loud, so it happens here where it can be read.
 */
fn shaped(route: &Route<'_>, body: &serde_json::Value) -> serde_json::Value {
    let Route::Own { deployment, .. } = route else { return body.clone() };
    if deployment.shape != Shape::Anthropic {
        return body.clone();
    }

    let mut shaped = body.clone();
    let Some(object) = shaped.as_object_mut() else { return shaped };

    if let Some(messages) = object.get_mut("messages").and_then(|m| m.as_array_mut()) {
        // Lifted out rather than dropped: left in the array it is ignored, and
        // the prompt that tells the model what it is doing is the last thing to
        // lose quietly.
        let system: Vec<String> = messages
            .iter()
            .filter(|m| m.get("role").and_then(|r| r.as_str()) == Some("system"))
            .filter_map(|m| m.get("content").and_then(|c| c.as_str()).map(String::from))
            .collect();
        messages.retain(|m| m.get("role").and_then(|r| r.as_str()) != Some("system"));
        if !system.is_empty() {
            object.insert("system".into(), system.join("\n\n").into());
        }
    }

    // Required by Anthropic and optional everywhere else. Generous, because
    // this cuts a summary off rather than refusing it.
    object.entry("max_tokens").or_insert_with(|| ANTHROPIC_MAX_TOKENS.into());
    // Not a parameter it knows; sent by the caller for the services that do.
    object.remove("response_format");
    shaped
}

/// Anthropic will not choose a length for us, so this does.
const ANTHROPIC_MAX_TOKENS: u32 = 8192;

/// Digs the message content out of a chat completion.
fn content_of(payload: &str) -> Result<String> {
    let parsed: serde_json::Value = serde_json::from_str(payload).map_err(|error| {
        Failure::Unexpected(format!("the model response was not JSON: {error}"))
    })?;

    // Two places to look, and no way to tell them apart from the payload alone
    // beyond trying. Anthropic answers `content[0].text`; everybody else
    // answers `choices[0].message.content`. Reading only the second turned a
    // good Claude answer into a model with nothing to say.
    let openai = parsed
        .get("choices")
        .and_then(|choices| choices.get(0))
        .and_then(|choice| choice.get("message"))
        .and_then(|message| message.get("content"))
        .and_then(|content| content.as_str());

    let anthropic = parsed
        .get("content")
        .and_then(|content| content.as_array())
        .map(|blocks| {
            blocks
                .iter()
                .filter(|block| block.get("type").and_then(|t| t.as_str()) != Some("thinking"))
                .filter_map(|block| block.get("text").and_then(|t| t.as_str()))
                .collect::<Vec<_>>()
                .join("")
        });

    Ok(openai
        .map(String::from)
        .or(anthropic)
        .unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn deployment(endpoint: &str, shape: Shape) -> Deployment {
        Deployment {
            endpoint: endpoint.into(),
            model: "a-model".into(),
            api_version: String::new(),
            shape,
        }
    }

    /// Only Azure versions its API in the query string. Appending it elsewhere
    /// is ignored at best and a 404 at worst.
    #[test]
    fn only_azure_gets_an_api_version() {
        assert_eq!(
            endpoint_url("https://api.openai.com/v1", "", Shape::OpenAi),
            "https://api.openai.com/v1/chat/completions"
        );
        assert_eq!(
            endpoint_url("http://localhost:11434/v1", "", Shape::OpenAi),
            "http://localhost:11434/v1/chat/completions"
        );
        assert!(endpoint_url("https://x.openai.azure.com", "2024-05-01-preview", Shape::Azure)
            .contains("?api-version="));
    }

    /// Anthropic's path is not a chat completion, and a church may type the
    /// base with or without the version segment.
    #[test]
    fn anthropic_is_addressed_at_messages() {
        for typed in ["https://api.anthropic.com", "https://api.anthropic.com/v1", "https://api.anthropic.com/v1/messages"] {
            assert_eq!(
                endpoint_url(typed, "", Shape::Anthropic),
                "https://api.anthropic.com/v1/messages",
                "{typed}"
            );
        }
    }

    /// The system prompt is lifted into its own field rather than left in the
    /// array, where Anthropic ignores it -- and losing the prompt that says
    /// what the model is doing is the quietest failure here.
    #[test]
    fn anthropic_takes_the_system_prompt_out_of_the_messages() {
        let deployment = deployment("https://api.anthropic.com", Shape::Anthropic);
        let route = Route::Own { deployment: &deployment, key: "k" };
        let body = serde_json::json!({
            "model": "claude",
            "messages": [
                {"role": "system", "content": "You summarise sermons."},
                {"role": "user", "content": "the transcript"}
            ],
            "response_format": {"type": "json_object"}
        });

        let sent = shaped(&route, &body);
        assert_eq!(sent["system"], "You summarise sermons.");
        assert_eq!(sent["messages"].as_array().unwrap().len(), 1);
        assert_eq!(sent["messages"][0]["role"], "user");
        // Required by Anthropic, and a 400 without it.
        assert!(sent["max_tokens"].is_number());
        // Not a parameter it knows.
        assert!(sent.get("response_format").is_none());
    }

    /// Everybody else gets the body exactly as it was built.
    #[test]
    fn the_other_shapes_are_sent_unchanged() {
        for shape in [Shape::OpenAi, Shape::Azure] {
            let deployment = deployment("https://api.openai.com/v1", shape);
            let route = Route::Own { deployment: &deployment, key: "k" };
            let body = serde_json::json!({"messages": [{"role": "system", "content": "s"}]});
            assert_eq!(shaped(&route, &body), body, "{shape:?}");
        }
    }

    /// Both reply shapes read, because the wrong one turns a good answer into
    /// a model with nothing to say.
    #[test]
    fn an_answer_is_read_from_either_shape() {
        assert_eq!(
            content_of(r#"{"choices":[{"message":{"content":"from openai"}}]}"#).unwrap(),
            "from openai"
        );
        assert_eq!(
            content_of(r#"{"content":[{"type":"text","text":"from claude"}]}"#).unwrap(),
            "from claude"
        );
        // Thinking blocks are not the answer.
        assert_eq!(
            content_of(r#"{"content":[{"type":"thinking","text":"hmm"},{"type":"text","text":"the answer"}]}"#)
                .unwrap(),
            "the answer"
        );
    }

    /// Plain HTTP reaches a model on this machine and nothing else. Anywhere
    /// but loopback it is a key and a sermon in clear across a church network.
    #[test]
    fn plain_http_is_for_this_machine_only() {
        for allowed in [
            "http://localhost:11434/v1",
            "http://127.0.0.1:1234/v1",
            "https://api.openai.com/v1",
        ] {
            assert!(transport_is_safe(allowed), "{allowed}");
        }
        for refused in [
            "http://192.168.1.50:11434/v1",
            "http://models.example.com/v1",
            "http://localhost.evil.com/v1",
            "ftp://localhost/v1",
        ] {
            assert!(!transport_is_safe(refused), "{refused}");
        }
    }

    #[test]
    fn reads_a_wait_the_service_asked_for() {
        assert_eq!(retry_after(r#"{"reason":"busy","retryAfter":12}"#), Some(12));
        assert_eq!(retry_after(r#"{"reason":"busy","retry_after":"7"}"#), Some(7));
        assert_eq!(retry_after(r#"{"reason":"busy"}"#), None);
        assert_eq!(retry_after("not json at all"), None);
    }

    #[test]
    fn recognises_a_network_that_intercepts_https() {
        for detail in [
            "invalid peer certificate: UnknownIssuer",
            "certificate verify failed: unable to get local issuer certificate",
        ] {
            assert!(is_intercepted(detail), "not recognised: {detail}");
        }
        assert!(!is_intercepted("dns error: failed to lookup address"));
        assert!(!is_intercepted("operation timed out"));
    }

    #[test]
    fn recognises_the_safety_filter() {
        for payload in [
            r#"{"error":{"code":"content_filter","message":"triggering Azure OpenAI's content management policy"}}"#,
            r#"{"error":"assistant_failed","reason":"filtered","status":400}"#,
        ] {
            assert!(is_filtered_refusal(payload), "not recognised: {payload}");
        }
        assert!(!is_filtered_refusal(r#"{"reason":"too_long"}"#));
    }

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
            // What our own broker sends on a subscription.
            r#"{"error":"assistant_failed","reason":"too_long","status":400}"#,
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
            endpoint_url("https://church-gpt.services.ai.azure.com", "2024-05-01-preview", Shape::Azure),
            "https://church-gpt.services.ai.azure.com/models/chat/completions?api-version=2024-05-01-preview"
        );
        // Trailing slash, same answer.
        assert_eq!(
            endpoint_url("https://church-gpt.services.ai.azure.com/", "v1", Shape::Azure),
            "https://church-gpt.services.ai.azure.com/models/chat/completions?api-version=v1"
        );
    }

    #[test]
    fn a_route_the_operator_already_named_is_left_alone() {
        assert!(endpoint_url("https://x.services.ai.azure.com/models", "v1", Shape::Azure)
            .starts_with("https://x.services.ai.azure.com/models/chat/completions"));
        // Azure OpenAI is a different shape and must not be rewritten.
        let openai = endpoint_url("https://x.openai.azure.com/openai/deployments/gpt/", "v1", Shape::Azure);
        assert!(!openai.contains("/models"), "{openai}");
    }

    #[test]
    fn a_host_that_is_not_foundry_is_never_rewritten() {
        let other = endpoint_url("https://api.example.test/v1", "2024-05-01-preview", Shape::Azure);
        assert_eq!(other, "https://api.example.test/v1/chat/completions?api-version=2024-05-01-preview");
    }

    #[test]
    fn an_empty_version_falls_back_rather_than_sending_nothing() {
        assert!(endpoint_url("https://api.example.test", "   ", Shape::Azure)
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
                    shape: Shape::Azure,
        };
        assert!(Route::Own { deployment: &full, key: "k" }.usable());
        // A deployment without a key is a deployment nobody can call.
        assert!(!Route::Own { deployment: &full, key: "  " }.usable());

        assert!(Route::Hosted { base: "https://www.castavox.com", device_token: "t" }.usable());
        assert!(!Route::Hosted { base: "https://www.castavox.com", device_token: "" }.usable());
    }
}
