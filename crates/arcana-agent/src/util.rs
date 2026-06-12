use crate::backend::LlmBackend;
use crate::error::{AgentError, Result};
use crate::types::{Message, SystemPrompt, Usage};

/// Truncate a string to at most `n` Unicode scalar values (chars), not bytes.
///
/// Byte-slicing (`&s[..n]`) panics when `n` lands in the middle of a multi-byte
/// UTF-8 sequence — and our own output routinely contains multi-byte glyphs
/// (`—`, `→`) because the style guide asks the model to emit them. Truncating on
/// `char` boundaries is panic-safe for any input.
pub(crate) fn truncate_chars(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

/// Extract the first *balanced* JSON object or array from LLM output.
///
/// Improves on naive first-`{`..last-`}`: it scans with brace-depth tracking and
/// ignores braces inside string literals, so trailing prose containing a `}`
/// (`{...} — see the } above`) no longer over-captures. Code fences fall away for
/// free because the scan starts at the first `{`/`[` and stops at its match. All
/// JSON structural characters are ASCII, so byte scanning stays UTF-8-safe.
pub(crate) fn extract_json(text: &str) -> &str {
    let trimmed = text.trim();
    let Some(start) = trimmed.find(['{', '[']) else {
        return trimmed;
    };
    let bytes = trimmed.as_bytes();
    let open = bytes[start];
    let close = if open == b'{' { b'}' } else { b']' };

    let mut depth = 0i32;
    let mut in_string = false;
    let mut escaped = false;
    for (offset, &c) in bytes[start..].iter().enumerate() {
        if in_string {
            if escaped {
                escaped = false;
            } else if c == b'\\' {
                escaped = true;
            } else if c == b'"' {
                in_string = false;
            }
            continue;
        }
        match c {
            b'"' => in_string = true,
            _ if c == open => depth += 1,
            _ if c == close => {
                depth -= 1;
                if depth == 0 {
                    return &trimmed[start..=start + offset];
                }
            }
            _ => {}
        }
    }
    // Unbalanced — best effort: from the first bracket to the end.
    &trimmed[start..]
}

/// Remove commas that immediately precede a closing `}` or `]` (skipping
/// whitespace), which `serde_json` rejects but models routinely emit. Commas
/// inside string literals are preserved.
fn strip_trailing_commas(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len());
    let mut in_string = false;
    let mut escaped = false;
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if in_string {
            out.push(c);
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_string = false;
            }
        } else if c == '"' {
            in_string = true;
            out.push(c);
        } else if c == ',' {
            let mut j = i + 1;
            while j < chars.len() && chars[j].is_whitespace() {
                j += 1;
            }
            // Drop the comma only if the next non-space char closes a container.
            if !matches!(chars.get(j), Some('}') | Some(']')) {
                out.push(c);
            }
        } else {
            out.push(c);
        }
        i += 1;
    }
    out
}

/// Extract and parse JSON from raw LLM text, tolerating code fences, trailing
/// prose, and trailing commas.
pub(crate) fn parse_json<T: serde::de::DeserializeOwned>(text: &str) -> serde_json::Result<T> {
    let cleaned = strip_trailing_commas(extract_json(text));
    serde_json::from_str(&cleaned)
}

/// Run a single LLM call expected to return JSON of type `T`, with robust
/// extraction and *one* reprompt that feeds the parse error back to the model.
/// Returns the parsed value plus the combined usage of both calls. `context`
/// labels the artifact in logs and error messages (e.g. `"ingest plan"`).
pub(crate) async fn chat_for_json<T: serde::de::DeserializeOwned>(
    llm: &dyn LlmBackend,
    system: &SystemPrompt,
    user_msg: String,
    context: &str,
) -> Result<(T, Usage)> {
    let mut usage = Usage::default();
    let mut messages = vec![Message::user(user_msg)];

    let response = llm.chat(system, &messages, &[]).await?;
    usage.accumulate(&response.usage);
    let text = response.text();
    tracing::debug!("{context} response: {}", truncate_chars(&text, 500));

    let first_err = match parse_json::<T>(&text) {
        Ok(value) => return Ok((value, usage)),
        Err(e) => e,
    };

    tracing::warn!("{context}: JSON parse failed ({first_err}); reprompting once");
    messages.push(Message::assistant(response.content));
    messages.push(Message::user(format!(
        "Your previous reply could not be parsed as JSON: {first_err}. \
         Reply with ONLY the corrected JSON — no prose, no markdown fences, no \
         trailing commas."
    )));

    let retry = llm.chat(system, &messages, &[]).await?;
    usage.accumulate(&retry.usage);
    let retry_text = retry.text();
    let value = parse_json::<T>(&retry_text).map_err(|e| {
        AgentError::Llm(format!(
            "failed to parse {context} JSON after reprompt: {e}\n\nraw response:\n{retry_text}"
        ))
    })?;
    Ok((value, usage))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_chars_splits_on_char_boundary() {
        // Each em-dash is 3 bytes; byte-slicing at 2 would panic mid-codepoint.
        let s = "a—b—c";
        assert_eq!(truncate_chars(s, 2), "a—");
        assert_eq!(truncate_chars(s, 1), "a");
        // n past the end returns the whole string, no panic.
        assert_eq!(truncate_chars(s, 999), s);
        // Pure multi-byte input (arrows) truncates cleanly.
        assert_eq!(truncate_chars("→→→", 2), "→→");
        assert_eq!(truncate_chars("", 5), "");
    }

    #[test]
    fn extract_json_plain() {
        assert_eq!(extract_json(r#"{"a": 1}"#), r#"{"a": 1}"#);
    }

    #[test]
    fn extract_json_with_fences() {
        let input = "```json\n{\"a\": 1}\n```";
        assert_eq!(extract_json(input), r#"{"a": 1}"#);
    }

    #[test]
    fn extract_json_with_bare_fences() {
        let input = "```\n{\"a\": 1}\n```";
        assert_eq!(extract_json(input), r#"{"a": 1}"#);
    }

    #[test]
    fn extract_json_with_preamble() {
        let input = "Here is the plan:\n{\"actions\": []}";
        assert_eq!(extract_json(input), r#"{"actions": []}"#);
    }

    #[test]
    fn extract_json_stops_at_balanced_close() {
        // Trailing prose containing a brace must not extend the capture.
        let input = "{\"a\": 1} Done — see the } above.";
        assert_eq!(extract_json(input), r#"{"a": 1}"#);
    }

    #[test]
    fn extract_json_ignores_braces_inside_strings() {
        let input = r#"{"a": "x } y"}"#;
        assert_eq!(extract_json(input), r#"{"a": "x } y"}"#);
    }

    #[test]
    fn parse_json_handles_fences() {
        let v: serde_json::Value = parse_json("```json\n{\"a\": 1}\n```").unwrap();
        assert_eq!(v["a"], 1);
    }

    #[test]
    fn parse_json_ignores_trailing_prose_with_braces() {
        let v: serde_json::Value = parse_json("{\"a\": 1} Done — see the } above.").unwrap();
        assert_eq!(v["a"], 1);
    }

    #[test]
    fn parse_json_tolerates_trailing_commas() {
        let v: serde_json::Value = parse_json("{\"a\": 1, \"b\": [2, 3,],}").unwrap();
        assert_eq!(v["a"], 1);
        assert_eq!(v["b"][1], 3);
    }

    #[test]
    fn parse_json_keeps_commas_inside_strings() {
        // A comma-before-brace *inside a string* must survive.
        let v: serde_json::Value = parse_json(r#"{"a": "x, }"}"#).unwrap();
        assert_eq!(v["a"], "x, }");
    }

    #[tokio::test]
    async fn chat_for_json_reprompts_once_on_bad_json() {
        use crate::backend::mock::MockBackend;
        use crate::types::{ContentBlock, LlmResponse, StopReason};

        let mock = MockBackend::new(vec![
            LlmResponse {
                content: vec![ContentBlock::Text {
                    text: "sorry, here are my thoughts but no json".into(),
                }],
                stop_reason: StopReason::EndTurn,
                usage: Usage {
                    input_tokens: 10,
                    output_tokens: 5,
                    ..Default::default()
                },
            },
            LlmResponse {
                content: vec![ContentBlock::Text {
                    text: "{\"a\": 7}".into(),
                }],
                stop_reason: StopReason::EndTurn,
                usage: Usage {
                    input_tokens: 12,
                    output_tokens: 4,
                    ..Default::default()
                },
            },
        ]);

        let (v, usage): (serde_json::Value, Usage) =
            chat_for_json(&mock, &SystemPrompt::default(), "go".into(), "test")
                .await
                .unwrap();
        assert_eq!(v["a"], 7);
        // Usage spans both calls.
        assert_eq!(usage.input_tokens, 22);
        assert_eq!(usage.output_tokens, 9);
    }
}
