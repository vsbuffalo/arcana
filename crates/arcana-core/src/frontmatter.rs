/// Split YAML frontmatter from markdown content.
///
/// Returns `(raw_yaml_string, body)`. If no frontmatter is found,
/// returns `("", full_content)`.
pub fn split_frontmatter(content: &str) -> (String, String) {
    let trimmed = content.trim_start();
    if !trimmed.starts_with("---") {
        return (String::new(), content.to_string());
    }

    let after_open = &trimmed[3..];
    let after_open = after_open.strip_prefix('\n').unwrap_or(after_open);

    // Handle empty frontmatter: "---\n---\n"
    if let Some(rest) = after_open.strip_prefix("---") {
        let body = rest.strip_prefix('\n').unwrap_or(rest).to_string();
        return (String::new(), body);
    }

    if let Some(end) = after_open.find("\n---") {
        let yaml_str = &after_open[..end];
        let body_start = end + 4; // skip \n---
        let body = if body_start < after_open.len() {
            let rest = &after_open[body_start..];
            rest.strip_prefix('\n').unwrap_or(rest).to_string()
        } else {
            String::new()
        };
        (yaml_str.to_string(), body)
    } else {
        // No closing ---, treat entire content as body
        (String::new(), content.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn with_frontmatter() {
        let content = "---\ntitle: Hello\ntags: [a]\n---\nBody here\n";
        let (yaml, body) = split_frontmatter(content);
        assert_eq!(yaml, "title: Hello\ntags: [a]");
        assert_eq!(body, "Body here\n");
    }

    #[test]
    fn without_frontmatter() {
        let content = "Just plain text\n";
        let (yaml, body) = split_frontmatter(content);
        assert_eq!(yaml, "");
        assert_eq!(body, content);
    }

    #[test]
    fn empty_frontmatter() {
        let content = "---\n---\nBody\n";
        let (yaml, body) = split_frontmatter(content);
        assert_eq!(yaml, "");
        assert_eq!(body, "Body\n");
    }

    #[test]
    fn no_closing_fence() {
        let content = "---\ntitle: Broken\nno closing fence\n";
        let (yaml, body) = split_frontmatter(content);
        assert_eq!(yaml, "");
        assert_eq!(body, content);
    }

    #[test]
    fn frontmatter_only_no_body() {
        let content = "---\ntitle: Test\n---";
        let (yaml, body) = split_frontmatter(content);
        assert_eq!(yaml, "title: Test");
        assert_eq!(body, "");
    }
}
