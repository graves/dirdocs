use crate::content::truncate;
use anyhow::Context;
use awful_aj::{api, config::AwfulJadeConfig, template::ChatTemplate};
use handlebars::Handlebars;
use serde::Deserialize;
use serde_yaml as yaml;
use tokio::time::{Duration, sleep};
use tracing::{info, warn};

#[allow(non_snake_case)]
#[derive(Debug, Deserialize)]
pub(crate) struct ModelResp {
    pub fileDescription: String,

    #[serde(alias = "howMuchJoyDoesThisFileBringYou")]
    pub joyThisFileBrings: serde_json::Value,

    #[serde(alias = "emojiThatExpressesThisFilesPersonality")]
    pub personalityEmoji: String,
}

pub(crate) fn sanitize_for_yaml(s: &str) -> String {
    s.chars()
        .filter_map(|c| match c {
            '\n' | '\r' | '\t' => Some(c),
            _ if c.is_control() => None,
            '\u{2028}' | '\u{2029}' | '\u{FEFF}' => Some(' '),
            _ => Some(c),
        })
        .collect()
}

pub(crate) fn indent_for_yaml(s: &str, n: usize) -> String {
    if s.is_empty() {
        return String::new();
    }
    let pad = " ".repeat(n);
    s.lines()
        .map(|line| format!("{pad}{line}"))
        .collect::<Vec<_>>()
        .join("\n")
}

pub(crate) fn suppressed_block() -> String {
    String::from("[[binary content suppressed]]")
}

pub(crate) fn render_chat_template(
    hbs: &Handlebars<'_>,
    raw_template: &str,
    data: &impl serde::Serialize,
) -> anyhow::Result<ChatTemplate> {
    let rendered = hbs
        .render_template(raw_template, data)
        .context("Handlebars render failed")?;
    let tpl: ChatTemplate = yaml::from_str(&rendered).map_err(|e| {
        anyhow::anyhow!(
            "YAML -> ChatTemplate error: {e}\n{}",
            truncate(&rendered, 400)
        )
    })?;
    Ok(tpl)
}

/// Exponential backoff + jitter around `api::ask`.
/// - `max_attempts` includes the first try
/// - base: 300ms, factor 2x, cap 8s, plus 0–250ms jitter
pub(crate) async fn ask_with_retry(
    cfg: &AwfulJadeConfig,
    prompt: &str,
    tpl: &ChatTemplate,
    max_attempts: usize,
) -> anyhow::Result<String> {
    let base = Duration::from_millis(300);
    let cap = Duration::from_secs(8);

    for attempt in 1..=max_attempts {
        match api::ask(cfg, prompt.to_string(), tpl, None, None).await {
            Ok(answer) => {
                if attempt > 1 {
                    info!(attempt, "api::ask succeeded after retries");
                }
                return Ok(answer);
            }
            Err(e) => {
                let emsg = e.to_string();
                let is_last = attempt == max_attempts;
                warn!(attempt, error=%emsg, "api::ask failed");
                if is_last {
                    return Err(anyhow::anyhow!(emsg));
                }

                // backoff = min(base * 2^(attempt-1), cap) + jitter
                let exp: u32 = ((attempt - 1) as u32).min(16);
                let factor: u32 = 1u32 << exp;
                let mut delay = base.checked_mul(factor).unwrap_or(cap);
                if delay > cap {
                    delay = cap;
                }
                delay += jitter_0_to_250ms();

                info!(
                    attempt_next = attempt + 1,
                    delay_ms = delay.as_millis(),
                    "Retrying api::ask"
                );
                sleep(delay).await;
            }
        }
    }
    Err(anyhow::anyhow!("ask_with_retry: exhausted attempts"))
}

fn jitter_0_to_250ms() -> Duration {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    Duration::from_nanos((nanos % 250_000_000) as u64)
}

pub(crate) fn sanitize_description(input: &str) -> String {
    use regex::Regex;

    let mut s = input.trim().trim_matches(['"', '\'']).to_string();

    let re_lead = Regex::new(
        r#"(?i)^\s*this\s+(?:file|script|module|class|service|program|document|config(?:uration)?(?:\s+file)?|shell\s+script)\b[,:;\-\s]*"#,
    )
    .unwrap();
    s = re_lead.replace(&s, "").to_string();

    let re_verb = Regex::new(r#"(?i)^\s*(?:is|does|provides|contains)\b[,:;\-\s]*"#).unwrap();
    s = re_verb.replace(&s, "").to_string();

    capitalize_first_alpha(&s)
}

fn capitalize_first_alpha(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut capitalized = false;
    for ch in s.chars() {
        if !capitalized && ch.is_alphabetic() {
            for up in ch.to_uppercase() {
                out.push(up);
            }
            capitalized = true;
        } else {
            out.push(ch);
        }
    }
    out.trim().to_string()
}
