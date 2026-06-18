/// Composable system prompt assembly from brain profile + task context.
///
/// All LLM interactions build prompts through this module. The order matters —
/// models process tokens sequentially, so the most important context goes first:
///
/// 1. taxonomy (conceptual framework — routing decisions depend on this)
/// 2. style guide (output shape — exemplars before any generation)
/// 3. domain skill (optional, task-specific extraction methodology)
/// 4. task instructions (what to do now)
/// 5. vault context (existing notes for cross-linking awareness)
use crate::types::SystemPrompt;

pub fn build_system_prompt(
    taxonomy: Option<&str>,
    style: Option<&str>,
    domain_skill: Option<&str>,
    task: &str,
    vault_context: Option<&str>,
) -> SystemPrompt {
    let mut prefix = String::with_capacity(8192);

    if let Some(tax) = taxonomy {
        prefix.push_str("<taxonomy>\n");
        prefix.push_str(tax);
        prefix.push_str("\n</taxonomy>\n\n");
    }

    if let Some(sty) = style {
        prefix.push_str("<style_guide>\n");
        prefix.push_str(sty);
        prefix.push_str("\n</style_guide>\n\n");
    }

    if let Some(skill) = domain_skill {
        prefix.push_str("<domain_skill>\n");
        prefix.push_str(skill);
        prefix.push_str("\n</domain_skill>\n\n");
    }

    prefix.push_str("<task>\n");
    prefix.push_str(task);
    prefix.push_str("\n</task>");

    // The static prefix (taxonomy/style/skill/task) is invariant across a run and
    // carries the cache breakpoint. The per-call vault_context lands in the
    // dynamic suffix so it stays *after* the breakpoint and never invalidates it.
    SystemPrompt {
        cached_prefix: prefix,
        // Already formatted as <vault_context>...</vault_context>.
        dynamic_suffix: vault_context.unwrap_or_default().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_sections_present() {
        let sp = build_system_prompt(
            Some("zones and routing"),
            Some("voice and templates"),
            Some("scientific models"),
            "tidy these notes",
            Some("<vault_context>\nexisting notes\n</vault_context>"),
        );

        // The vault_context lands in the dynamic suffix; everything else is the
        // cacheable prefix.
        assert!(sp.cached_prefix.contains("<task>"));
        assert!(!sp.cached_prefix.contains("<vault_context>"));
        assert_eq!(
            sp.dynamic_suffix,
            "<vault_context>\nexisting notes\n</vault_context>"
        );

        // Verify order within the cached prefix: taxonomy → style → skill → task.
        let prefix = &sp.cached_prefix;
        let tax_pos = prefix.find("<taxonomy>").unwrap();
        let sty_pos = prefix.find("<style_guide>").unwrap();
        let skill_pos = prefix.find("<domain_skill>").unwrap();
        let task_pos = prefix.find("<task>").unwrap();
        assert!(tax_pos < sty_pos);
        assert!(sty_pos < skill_pos);
        assert!(skill_pos < task_pos);

        // full_text places the dynamic context last.
        let full = sp.full_text();
        assert!(full.find("<task>").unwrap() < full.find("<vault_context>").unwrap());
    }

    #[test]
    fn none_sections_omitted() {
        let sp = build_system_prompt(None, None, None, "just a task", None);

        assert!(!sp.cached_prefix.contains("<taxonomy>"));
        assert!(!sp.cached_prefix.contains("<style_guide>"));
        assert!(!sp.cached_prefix.contains("<domain_skill>"));
        assert!(sp.dynamic_suffix.is_empty());
        assert!(sp.cached_prefix.contains("<task>\njust a task\n</task>"));
    }

    #[test]
    fn partial_sections() {
        let sp = build_system_prompt(
            Some("my taxonomy"),
            None,
            None,
            "do the thing",
            Some("<vault_context>notes</vault_context>"),
        );

        assert!(sp.cached_prefix.contains("<taxonomy>"));
        assert!(!sp.cached_prefix.contains("<style_guide>"));
        assert!(sp.cached_prefix.contains("<task>"));
        assert_eq!(sp.dynamic_suffix, "<vault_context>notes</vault_context>");
    }

    #[test]
    fn task_is_always_present() {
        let sp = build_system_prompt(None, None, None, "", None);
        assert!(sp.cached_prefix.contains("<task>\n\n</task>"));
    }
}
