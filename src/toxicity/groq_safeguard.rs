//! Groq GPT-OSS-Safeguard scorer — policy-based content moderation.
//!
//! Calls Groq's chat completions API with a custom moderation policy
//! and returns structured violation assessments. Used as the secondary
//! scorer in the ensemble for two-way correction of ONNX scores.

use anyhow::{Context, Result};
use async_trait::async_trait;
use serde::Deserialize;
use std::sync::Arc;
use tokio::sync::{Mutex, Semaphore};
use tokio::time::{Duration, Instant};
use tracing::warn;

use super::traits::{ToxicityAttributes, ToxicityResult, ToxicityScorer};

const ENDPOINT: &str = "https://api.groq.com/openai/v1/chat/completions";
const MODEL: &str = "openai/gpt-oss-safeguard-20b";

const MAX_RETRIES: u32 = 5;
const INITIAL_BACKOFF_MS: u64 = 2000;
const MIN_REQUEST_INTERVAL: Duration = Duration::from_millis(2000);

/// Moderation policy sent as the system prompt.
const POLICY: &str = r#"# Charcoal Content Moderation Policy

## INSTRUCTIONS
Classify the following social media post for hostile or bad-faith content directed at or about other people. Return JSON with fields: violation (1 or 0), category (string), and rationale (string).

## DEFINITIONS
- "Hostile content" means language intended to harm, demean, intimidate, or silence a specific person or group.
- "Bad-faith engagement" means responses that attack the person rather than engaging with their ideas.

## VIOLATES (1)
- Identity-based hostility: slurs, dehumanization, hate speech targeting identity (race, gender, sexuality, body size, disability)
- Targeted harassment: direct insults, mockery, name-calling aimed at a specific person
- Threat language: explicit or implied threats of harm, doxxing, or encouraging others to harm someone
- Pile-on instigation: language designed to direct an audience at a target ("ratio this", "look at this idiot", quote-dunking)
- Bad-faith engagement: ad hominems, strawmanning, dismissive non-engagement ("you're just...", "people like you...")
- Category errors as attacks: reducing someone's argument to an identity trait ("you only think that because you're...")

## SAFE (0)
- Strong opinions or passionate political language without personal attacks
- Profanity not directed at a person
- Discussion of sensitive topics (body politics, identity, race) in supportive or analytical context
- Disagreement that engages with the substance of an argument
- Satire or humor not targeting a specific person

## RESPONSE FORMAT
Return a JSON object with exactly these fields:
{"violation": 1, "category": "Targeted harassment", "rationale": "..."}
{"violation": 0, "category": "none", "rationale": "..."}"#;

/// Parsed response from GPT-OSS-Safeguard.
#[derive(Debug, Clone)]
pub struct SafeguardResult {
    pub violation: bool,
    pub category: String,
    pub rationale: String,
}

// Serde helper: deserialize 0/1 as bool
mod violation_bool {
    use serde::{self, Deserialize, Deserializer};

    pub fn deserialize<'de, D>(deserializer: D) -> Result<bool, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum ViolationValue {
            Int(i64),
            Bool(bool),
        }
        match ViolationValue::deserialize(deserializer)? {
            ViolationValue::Int(n) => Ok(n != 0),
            ViolationValue::Bool(b) => Ok(b),
        }
    }
}

/// Raw serde struct for the Groq response.
#[derive(Debug, Deserialize)]
struct SafeguardResponseRaw {
    #[serde(deserialize_with = "violation_bool::deserialize")]
    violation: bool,
    category: String,
    rationale: String,
}

/// Parse the JSON content from a Groq safeguard response.
/// Returns None on any parse failure (fail safe — don't inflate scores).
pub fn parse_safeguard_response(content: &str) -> Option<SafeguardResult> {
    let raw: SafeguardResponseRaw = serde_json::from_str(content).ok()?;
    Some(SafeguardResult {
        violation: raw.violation,
        category: raw.category,
        rationale: raw.rationale,
    })
}

/// Get the boost factor for a Groq violation category.
pub fn boost_for_category(category: &str) -> f64 {
    match category {
        "Identity-based hostility" => 2.0,
        "Targeted harassment" => 2.0,
        "Threat language" => 2.0,
        "Pile-on instigation" => 1.75,
        "Bad-faith engagement" => 1.5,
        "Category errors as attacks" => 1.5,
        _ => 1.5,
    }
}

/// Map a Groq category to ToxicityAttributes.
fn attributes_for_category(category: &str) -> ToxicityAttributes {
    match category {
        "Identity-based hostility" => ToxicityAttributes {
            identity_attack: Some(1.0),
            ..Default::default()
        },
        "Threat language" => ToxicityAttributes {
            threat: Some(1.0),
            ..Default::default()
        },
        _ => ToxicityAttributes {
            insult: Some(1.0),
            ..Default::default()
        },
    }
}

#[derive(Debug, Deserialize)]
struct ChatChoice {
    message: ChatMessage,
    #[allow(dead_code)]
    finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ChatMessage {
    content: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ChatResponse {
    choices: Vec<ChatChoice>,
}

/// Groq GPT-OSS-Safeguard scorer.
pub struct GroqSafeguardScorer {
    client: reqwest::Client,
    api_key: String,
    semaphore: Arc<Semaphore>,
    last_request: Arc<Mutex<Instant>>,
}

impl GroqSafeguardScorer {
    pub fn new(api_key: &str) -> Result<Self> {
        let client = reqwest::Client::builder()
            .user_agent("charcoal/0.1 (threat-detection; @chaosgreml.in)")
            .build()
            .context("Failed to build HTTP client for Groq")?;

        Ok(Self {
            client,
            api_key: api_key.to_string(),
            semaphore: Arc::new(Semaphore::new(1)),
            last_request: Arc::new(Mutex::new(Instant::now() - MIN_REQUEST_INTERVAL)),
        })
    }

    async fn send_with_retry(&self, user_content: &str) -> Result<Option<SafeguardResult>> {
        let _permit = self
            .semaphore
            .acquire()
            .await
            .map_err(|e| anyhow::anyhow!("Semaphore closed: {e}"))?;

        let body = serde_json::json!({
            "model": MODEL,
            "messages": [
                { "role": "system", "content": POLICY },
                { "role": "user", "content": user_content }
            ],
            "response_format": { "type": "json_object" }
        });

        let mut backoff_ms = INITIAL_BACKOFF_MS;

        for attempt in 0..=MAX_RETRIES {
            {
                let mut last = self.last_request.lock().await;
                let elapsed = last.elapsed();
                if elapsed < MIN_REQUEST_INTERVAL {
                    tokio::time::sleep(MIN_REQUEST_INTERVAL - elapsed).await;
                }
                *last = Instant::now();
            }

            let response = self
                .client
                .post(ENDPOINT)
                .header("Authorization", format!("Bearer {}", self.api_key))
                .json(&body)
                .send()
                .await
                .context("Groq API request failed")?;

            let status = response.status();

            if status.as_u16() == 429 {
                if attempt < MAX_RETRIES {
                    warn!(
                        attempt = attempt + 1,
                        backoff_ms, "Groq rate limited (429), retrying"
                    );
                    tokio::time::sleep(Duration::from_millis(backoff_ms)).await;
                    backoff_ms *= 2;
                    continue;
                }
                let error_body = response.text().await.unwrap_or_default();
                anyhow::bail!("Groq API rate limited after {MAX_RETRIES} retries: {error_body}");
            }

            if !status.is_success() {
                let error_body = response.text().await.unwrap_or_default();
                anyhow::bail!("Groq API error {status}: {error_body}");
            }

            let chat_response: ChatResponse = response
                .json()
                .await
                .context("Failed to parse Groq chat response")?;

            let content = chat_response
                .choices
                .first()
                .and_then(|c| c.message.content.as_deref());

            return Ok(content.and_then(parse_safeguard_response));
        }

        anyhow::bail!("Groq API: exhausted retries")
    }

    fn format_user_message(text: &str, context: Option<&str>) -> String {
        match context {
            Some(original) => format!(
                "Original post by protected user:\n\"{original}\"\n\nResponse being evaluated:\n\"{text}\""
            ),
            None => format!("Post being evaluated:\n\"{text}\""),
        }
    }

    /// Score text with optional context, returning the SafeguardResult.
    pub async fn score_with_safeguard(
        &self,
        text: &str,
        context: Option<&str>,
    ) -> Result<Option<SafeguardResult>> {
        let user_message = Self::format_user_message(text, context);
        self.send_with_retry(&user_message).await
    }
}

fn safeguard_to_toxicity(result: &Option<SafeguardResult>) -> ToxicityResult {
    match result {
        Some(sr) if sr.violation => {
            let attrs = attributes_for_category(&sr.category);
            ToxicityResult {
                toxicity: 1.0,
                attributes: attrs,
            }
        }
        _ => ToxicityResult {
            toxicity: 0.0,
            attributes: ToxicityAttributes::default(),
        },
    }
}

#[async_trait]
impl ToxicityScorer for GroqSafeguardScorer {
    async fn score_text(&self, text: &str) -> Result<ToxicityResult> {
        let result = self.score_with_safeguard(text, None).await?;
        Ok(safeguard_to_toxicity(&result))
    }

    async fn score_with_context(
        &self,
        text: &str,
        context: Option<&str>,
    ) -> Result<ToxicityResult> {
        let result = self.score_with_safeguard(text, context).await?;
        Ok(safeguard_to_toxicity(&result))
    }
}
