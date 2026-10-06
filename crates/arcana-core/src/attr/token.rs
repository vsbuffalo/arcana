//! Tokenizer for attribution: the units that carry authorship.
//!
//! Words, punctuation and markdown markup are tokens; whitespace is only a
//! separator. In CommonMark a newline inside a paragraph is the same as a
//! space, so re-wrapping a paragraph produces the same token sequence and
//! changes nobody's authorship. Inside fenced code, whitespace is content, so
//! each non-blank line is one token.

/// Version tag stored in every sidecar. Changing tokenization rules requires a
/// new version, so old sidecars are never silently re-aligned.
pub const TOKENIZER: &str = "tok-1";

/// A token as a byte range into the content it was cut from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Token {
    pub start: usize,
    pub end: usize,
}

impl Token {
    pub fn text<'a>(&self, content: &'a str) -> &'a str {
        &content[self.start..self.end]
    }
}

/// Split `content` into attribution tokens.
pub fn tokenize(content: &str) -> Vec<Token> {
    let mut tokens = Vec::new();
    let mut fence: Option<&str> = None;
    let mut line_start = 0;

    for line in content.split_inclusive('\n') {
        let body = line.trim_end_matches(['\n', '\r']);
        let opener = fence_marker(body);
        match (fence, opener) {
            (None, Some(marker)) => {
                fence = Some(marker);
                push_line(&mut tokens, body, line_start);
            }
            (Some(open), Some(marker)) if marker.starts_with(open) && is_fence_close(body) => {
                fence = None;
                push_line(&mut tokens, body, line_start);
            }
            (Some(_), _) => push_line(&mut tokens, body, line_start),
            (None, None) => push_prose(&mut tokens, body, line_start),
        }
        line_start += line.len();
    }
    tokens
}

/// The fence marker (``` or ~~~, possibly longer) opening a line, if any.
fn fence_marker(line: &str) -> Option<&str> {
    let trimmed = line.trim_start_matches(' ');
    if line.len() - trimmed.len() > 3 {
        return None;
    }
    for ch in ['`', '~'] {
        let n = trimmed.chars().take_while(|&c| c == ch).count();
        if n >= 3 {
            return Some(&trimmed[..n]);
        }
    }
    None
}

fn is_fence_close(line: &str) -> bool {
    let t = line.trim();
    t.chars().all(|c| c == '`') || t.chars().all(|c| c == '~')
}

/// One token for a whole code line (without leading or trailing whitespace).
fn push_line(tokens: &mut Vec<Token>, body: &str, offset: usize) {
    let lead = body.len() - body.trim_start().len();
    let trimmed = body.trim();
    if !trimmed.is_empty() {
        tokens.push(Token {
            start: offset + lead,
            end: offset + lead + trimmed.len(),
        });
    }
}

/// Words (alphanumeric runs, with internal apostrophes) and single-character
/// punctuation or markup tokens.
fn push_prose(tokens: &mut Vec<Token>, body: &str, offset: usize) {
    let chars: Vec<(usize, char)> = body.char_indices().collect();
    let mut i = 0;
    while i < chars.len() {
        let (pos, c) = chars[i];
        if c.is_whitespace() {
            i += 1;
            continue;
        }
        if is_word_char(c) {
            let start = pos;
            let mut j = i + 1;
            while j < chars.len() {
                let (_, cj) = chars[j];
                let apostrophe =
                    matches!(cj, '\'' | '’') && j + 1 < chars.len() && is_word_char(chars[j + 1].1);
                if is_word_char(cj) || apostrophe {
                    j += 1;
                } else {
                    break;
                }
            }
            let end = chars.get(j).map_or(body.len(), |&(p, _)| p);
            tokens.push(Token {
                start: offset + start,
                end: offset + end,
            });
            i = j;
        } else {
            tokens.push(Token {
                start: offset + pos,
                end: offset + pos + c.len_utf8(),
            });
            i += 1;
        }
    }
}

fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(s: &str) -> Vec<&str> {
        tokenize(s).iter().map(|t| t.text(s)).collect()
    }

    #[test]
    fn words_and_punctuation() {
        assert_eq!(
            texts("Resonance isn't second-order."),
            ["Resonance", "isn't", "second", "-", "order", "."]
        );
    }

    #[test]
    fn rewrap_is_token_neutral() {
        let a = "Resonance is the response of any linear second-order system.";
        let b = "Resonance is the response of any\nlinear second-order\n   system.";
        assert_eq!(texts(a), texts(b));
    }

    #[test]
    fn markup_is_tokens() {
        assert_eq!(texts("## Q *factor* [@french1971]")[..3], ["#", "#", "Q"]);
    }

    #[test]
    fn code_fence_lines_are_single_tokens() {
        let s = "text here\n```rust\nlet x = 1;\n    y += 2;\n```\nafter";
        assert_eq!(
            texts(s),
            [
                "text",
                "here",
                "```rust",
                "let x = 1;",
                "y += 2;",
                "```",
                "after"
            ]
        );
    }

    #[test]
    fn unicode_words() {
        assert_eq!(texts("Δω = ω₀/Q"), ["Δω", "=", "ω₀", "/", "Q"]);
    }
}
