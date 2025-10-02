use crate::content::truncate;
use anyhow::Context;
use awful_aj::{api, config::AwfulJadeConfig, template::ChatTemplate};
use handlebars::Handlebars;
use serde::Deserialize;
use serde_yaml as yaml;
use tokio::time::{Duration, sleep};
use tracing::{info, warn};

/// ModelResp represents a response from the LLM about a file, including its description, joy level, and personality emoji.
#[allow(non_snake_case)]
#[derive(Debug, Deserialize)]
pub(crate) struct ModelResp {
    /// This is the file description, storing the human-readable name of a file.
    pub fileDescription: String,

    /// This value represents how much joy the file brings, stored as a JSON string.
    #[serde(alias = "howMuchJoyDoesThisFileBringYou")]
    pub joyThisFileBrings: serde_json::Value,

    /// This string represents the personality emoji of a file.
    #[serde(alias = "emojiThatExpressesThisFilesPersonality")]
    pub personalityEmoji: String,
}

/// Sanitizes a string for safe YAML serialization by filtering out control characters
/// and replacing certain Unicode line breaks with spaces. This is useful for
/// ensuring that strings can be safely written to YAML files without corruption.
///
/// Parameters:
/// - `s`: The input string to sanitize.
///
/// Returns:
/// A new `String`` with sanitized content, containing only valid YAML characters
/// and appropriate replacements for control sequences.
///
/// Errors:
/// - This function does not return any errors directly; it handles all logic at the
///   call site.
///
/// Notes:
/// - This function is intended for use in contexts where strings may contain
///   special characters that need to be safely represented.
/// - The `is_control` method is used internally for character classification.
///
/// The implementation filters out newlines (`
/// `), carriage returns (`\r`),
/// and tabs (`	`). It replaces Unicode line breaks (U+2028, U+2029) and
/// the zero-width space (U+FEFF) with spaces. All other control characters
/// are removed, and non-control characters are retained unchanged.
///
/// The `filter_map` method is used to process each character, and the
/// `collect()` function combines all valid characters into a final String.
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

/// Indents a string by `n` spaces, with each line in the string indented.
///
/// # Parameters:
/// - `s`: The input string to indent.
/// - `n`: The number of spaces to add as indentation.
///
/// # Returns:
/// A new `String` with each line indented by `n` spaces.
///
/// # Notes:
/// - If the input string is empty, it returns an empty string.
/// - The function handles line breaks and indents each line individually.
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
/// Returns a string representation for suppressed binary content.
///
/// This function returns the literal "[[binary content suppressed]]" as a `String`.
///
/// Returns:
/// - Always returns the string "[[binary content suppressed]]".
pub(crate) fn suppressed_block() -> String {
    String::from("[[binary content suppressed]]")
}

/// Handlebars-rendered chat template.
///
/// Renders a raw text template using the given `Handlebars` instance and data,
/// serializes the result to YAML, then maps it back into a `ChatTemplate`.
///
/// Parameters:
/// - `hbs`: A reference to the Handlebars instance.
/// - `raw_template`: The raw text template as a string slice.
/// - `data`: A reference to data that implements the `serde::Serialize` trait.
///
/// Returns:
/// - A serialized `ChatTemplate`, or an error if serializing, rendering,
///   or parsing fails.
///
/// Errors:
/// - All errors from `Handlebars::render_template`, `yaml::from_str`, and
///   `anyhow::anyhow` are returned.
///
/// Safety:
/// - This function is safe to call as it only handles references and
///   does not perform unsafe operations.
///
/// Notes:
/// - If the rendered output is too long, it will be truncated and logged
///   with an error message.
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
/// This function attempts to call `api::ask` with increasing delay between retries, up to a maximum number of attempts.
/// It uses exponential jitter for randomization in delay time and handles failures gracefully by retrying.
///
/// Parameters:
/// - `cfg`: A reference to an [`AwfulJadeConfig`] instance.
/// - `prompt`: The user input string to send via the API.
/// - `tpl`: A reference to a [`ChatTemplate`] used for formatting the request.
/// - `max_attempts`: The maximum number of retry attempts allowed (including the initial call).
///
/// Returns:
/// - `Ok(String)`: The response from `api::ask` if successful.
/// - `Err(anyhow::Error)`: If all attempts fail, the error is returned with a description of the failure.
///
/// Errors:
/// - I/O errors during file operations (if applicable).
/// - API errors from `api::ask`.
/// - Serialization or parsing errors (e.g., invalid JSON/YAML).
/// - Exceeded maximum attempts.
///
/// Safety:
/// This function is `pub(crate)`, meaning it's intended for internal use within the crate.
///
/// Notes:
/// - The backoff delay increases exponentially, capped at 8 seconds.
/// - Jitter (0–250ms) is added to prevent repeated retries with identical delays.
/// - The initial call (attempt 1) does not have a delay, and subsequent errors trigger retries.
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

/// Adds a random jitter between 0 and 250 milliseconds to the current system time.
/// This function calculates a duration based on subsecond nanoseconds of the current timestamp, applying modulo to ensure it falls within 0-250ms.
/// The result is returned as a `Duration`.
///
/// Parameters:
/// - None.
///
/// Returns:
/// - A random jitter amount between 0 and 250 milliseconds as a `Duration`.
///
/// Errors:
/// - None. This function does not return an error.
///
/// Notes:
/// - This function is designed to introduce slight variability in timing, useful for avoiding strict synchronization.
/// - The jitter is calculated using the current system time and is not guaranteed to be exactly random but rather evenly distributed.
fn jitter_0_to_250ms() -> Duration {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    Duration::from_nanos((nanos % 250_000_000) as u64)
}

/// Sanitizes a description by removing leading phrases and capitalizing the first letter.
///
/// This function trims input, removes leading text matching patterns like "this file",
/// and capitalizes the first letter of the resulting string. It uses regular expressions
/// to identify and strip out common introductory phrases.
///
/// Parameters:
/// - `input`: A reference to a string slice (`&str`) to sanitize.
///
/// Returns:
/// - A new `String` with leading text removed and first letter capitalized.
///
/// Errors:
/// - None, as this function does not return errors explicitly. If regex initialization
/// fails, it panics (since calls to `unwrap()` within the function body are used).
///
/// Notes:
/// - This function is useful for standardizing descriptions, such as in documentation
/// or user prompts.
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

/// Capitalizes the first alphabetic character in a string and leaves the rest unchanged.
///
/// Parameters:
/// - `s`: A reference to a string slice (`&str`).
///
/// Returns:
/// - A new `String` with the first alphabetic character capitalized.
///
/// Safety:
/// - This function is safe to call as it only operates on valid UTF-8 input.
///
/// Notes:
/// - The function uses a `String::with_capacity` to efficiently allocate memory.
/// - It processes each character in the string, capitalizing only the first alphabetic one.
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
