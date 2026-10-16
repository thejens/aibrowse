use std::time::Duration;

use serde_json::{Value, json};

const ENDPOINT: &str = "https://api.typesafe.ai/v1/systemone";
/// The model asked unless `JEV_MODEL` names another (`jev-1.14.0`, `jev-preview`): a
/// new release can be measured on the evals before `jev-latest` moves to it.
const MODEL: &str = "jev-latest";
const ATTEMPTS: u32 = 4;
/// How long one attempt may take. Jev answers in about 0.25 s, even sixty questions at
/// once (at most 0.6 s across two eval suites, 2026-10-09), so an attempt still waiting
/// after a few seconds has hung, and a fresh one is the quicker way to an answer: with
/// 20 s, one hung request and its retry stalled a pursuit for ~45 s.
const ATTEMPT_TIME: Duration = Duration::from_secs(5);

#[derive(Debug, thiserror::Error)]
pub enum JevError {
    #[error(
        "no Jev API key: set JEV_API_KEY, or put it in ~/.aibrowse/env (`make install` copies .env there)"
    )]
    NoKey,
    #[error("Jev request failed: {}", with_causes(.0))]
    Http(#[from] reqwest::Error),
    #[error("Jev returned {status}: {body}")]
    Api { status: u16, body: String },
}

/// Client for Jev's decision endpoint. One request asks many typed questions about one
/// state; asking them together (speculative fan-out) costs about the same as asking one.
#[derive(Clone)]
pub struct Jev {
    http: reqwest::Client,
    key: String,
    model: String,
}

impl Jev {
    /// Uses `JEV_API_KEY`, falling back to `~/.aibrowse/env` so a registered MCP server
    /// needn't carry the secret in its client's configuration.
    pub fn from_env() -> Result<Self, JevError> {
        let key = std::env::var("JEV_API_KEY")
            .ok()
            .or_else(key_from_file)
            .filter(|k| !k.trim().is_empty())
            .ok_or(JevError::NoKey)?;
        let http = reqwest::Client::builder().timeout(ATTEMPT_TIME).build()?;
        let model = std::env::var("JEV_MODEL")
            .ok()
            .filter(|m| !m.trim().is_empty())
            .unwrap_or_else(|| MODEL.to_owned());
        Ok(Self {
            http,
            key: key.trim().to_owned(),
            model: model.trim().to_owned(),
        })
    }

    /// Asks `questions` (a map of name → typed question) about `state` and returns the
    /// `answers` map. Retries transient failures with backoff: rate limiting (429),
    /// overload (529) and an unavailable upstream (502/503/504, seen as Jev's model
    /// briefly going away).
    pub async fn ask(&self, state: &Value, questions: &Value) -> Result<Value, JevError> {
        let body = json!({ "model": self.model, "state": state, "questions": questions });
        let mut delay = Duration::from_millis(400);
        for attempt in 1..=ATTEMPTS {
            let sending = std::time::Instant::now();
            let sent = self
                .http
                .post(ENDPOINT)
                .bearer_auth(&self.key)
                .json(&body)
                .send()
                .await;
            let response = match sent {
                Ok(response) => response,
                // A dropped or refused connection is as passing as a busy server: one
                // lost request shouldn't stop a pursuit or block an action.
                Err(err)
                    if (err.is_connect() || err.is_timeout() || err.is_request())
                        && attempt < ATTEMPTS =>
                {
                    tracing::debug!(%err, attempt, elapsed = ?sending.elapsed(), "Jev unreachable; retrying");
                    tokio::time::sleep(delay).await;
                    delay *= 2;
                    continue;
                }
                Err(err) => return Err(err.into()),
            };
            let status = response.status().as_u16();
            if status == 200 {
                let mut value: Value = response.json().await?;
                tracing::debug!(attempt, elapsed = ?sending.elapsed(), "Jev answered");
                return Ok(value["answers"].take());
            }
            let text = response.text().await.unwrap_or_default();
            if matches!(status, 429 | 502 | 503 | 504 | 529) && attempt < ATTEMPTS {
                tracing::debug!(status, attempt, "Jev busy; backing off");
                tokio::time::sleep(delay).await;
                delay *= 2;
                continue;
            }
            return Err(JevError::Api {
                status,
                body: text.chars().take(500).collect(),
            });
        }
        unreachable!("the last attempt always returns")
    }
}

/// An error with what caused it: reqwest's own message ("error sending request") leaves
/// out whether the connection was refused, reset or timed out.
fn with_causes(err: &reqwest::Error) -> String {
    let mut out = err.to_string();
    let mut source = std::error::Error::source(err);
    while let Some(cause) = source {
        out.push_str(": ");
        out.push_str(&cause.to_string());
        source = cause.source();
    }
    out
}

fn key_from_file() -> Option<String> {
    let text = std::fs::read_to_string(dirs::home_dir()?.join(".aibrowse/env")).ok()?;
    text.lines()
        .filter_map(|line| line.trim().strip_prefix("JEV_API_KEY="))
        .map(|v| v.trim_matches('"').to_owned())
        .next()
}

/// The chosen option of a `choice` answer and its confidence.
pub(crate) fn choice(answers: &Value, name: &str) -> (String, f64) {
    let answer = &answers[name];
    (
        answer["choice"].as_str().unwrap_or_default().to_owned(),
        answer["confidence"].as_f64().unwrap_or(0.0),
    )
}

/// The options of a `choice` answer, most probable first.
pub(crate) fn ranked(answers: &Value, name: &str) -> Vec<(String, f64)> {
    let mut options: Vec<(String, f64)> = answers[name]["probabilities"]
        .as_object()
        .map(|o| {
            o.iter()
                .map(|(k, v)| (k.clone(), v.as_f64().unwrap_or(0.0)))
                .collect()
        })
        .unwrap_or_default();
    options.sort_by(|a, b| b.1.total_cmp(&a.1));
    options
}
