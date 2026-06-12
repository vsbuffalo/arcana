/// Truncate a string to at most `n` Unicode scalar values (chars), not bytes.
///
/// Byte-slicing (`&s[..n]`) panics when `n` lands in the middle of a multi-byte
/// UTF-8 sequence — and our own output routinely contains multi-byte glyphs
/// (`—`, `→`) because the style guide asks the model to emit them. Truncating on
/// `char` boundaries is panic-safe for any input.
pub(crate) fn truncate_chars(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

/// Extract JSON from LLM response, stripping markdown code fences if present.
pub(crate) fn extract_json(text: &str) -> &str {
    let trimmed = text.trim();
    if let Some(start) = trimmed.find('{') {
        if let Some(end) = trimmed.rfind('}') {
            return &trimmed[start..=end];
        }
    }
    trimmed
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
}
