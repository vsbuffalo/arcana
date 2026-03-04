use arcana_core::{SearchQuery, Vault};

/// Generate a structured context block from vault search results.
/// Pure search + format, no LLM needed.
pub fn generate_context(vault: &Vault, query: &str, limit: usize) -> String {
    let search_query = SearchQuery {
        text: query.to_string(),
        limit: Some(limit),
        ..Default::default()
    };

    let results = match vault.search(&search_query) {
        Ok(r) => r,
        Err(e) => return format!("<vault_context error=\"{e}\" />\n"),
    };

    if results.is_empty() {
        return format!(
            "<vault_context query=\"{query}\">\nNo matching notes found.\n</vault_context>\n"
        );
    }

    let mut out = format!(
        "<vault_context query=\"{query}\" results=\"{}\">\n",
        results.len()
    );

    for result in &results {
        // Read full note for richer context
        let note_content = match vault.read_note(&result.path) {
            Ok(note) => {
                let title = note.title().to_string();
                let tags = if note.frontmatter.tags.is_empty() {
                    String::new()
                } else {
                    format!(" tags=\"{}\"", note.frontmatter.tags.join(", "))
                };

                // Truncate body to reasonable size for context
                let body = if note.body.len() > 2000 {
                    format!("{}...", &note.body[..2000])
                } else {
                    note.body.clone()
                };

                format!(
                    "<note path=\"{}\" title=\"{}\"{tags}>\n{body}\n</note>\n",
                    result.path, title,
                )
            }
            Err(_) => {
                // Fall back to snippet
                let snippet = result.snippet.replace("<mark>", "").replace("</mark>", "");
                format!(
                    "<note path=\"{}\" title=\"{}\">\n{snippet}\n</note>\n",
                    result.path,
                    result.title.as_deref().unwrap_or("Untitled"),
                )
            }
        };
        out.push_str(&note_content);
    }

    out.push_str("</vault_context>\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use arcana_core::ArcanaConfig;
    use std::path::PathBuf;

    fn fixture_vault_path() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../arcana-core/../../tests/fixtures/small_vault")
    }

    #[test]
    fn generate_context_with_results() {
        let config = ArcanaConfig::default().with_vault_path(fixture_vault_path());
        let vault = Vault::open_in_memory(config).unwrap();
        vault.index().unwrap();

        let ctx = generate_context(&vault, "rust", 5);
        assert!(ctx.contains("<vault_context"));
        assert!(ctx.contains("</vault_context>"));
        assert!(ctx.contains("<note"));
    }

    #[test]
    fn generate_context_no_results() {
        let config = ArcanaConfig::default().with_vault_path(fixture_vault_path());
        let vault = Vault::open_in_memory(config).unwrap();
        vault.index().unwrap();

        let ctx = generate_context(&vault, "xyznonexistent123", 5);
        assert!(ctx.contains("No matching notes found"));
    }
}
