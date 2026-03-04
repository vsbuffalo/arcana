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
pub fn build_system_prompt(
    taxonomy: Option<&str>,
    style: Option<&str>,
    domain_skill: Option<&str>,
    task: &str,
    vault_context: Option<&str>,
) -> String {
    let mut prompt = String::with_capacity(8192);

    if let Some(tax) = taxonomy {
        prompt.push_str("<taxonomy>\n");
        prompt.push_str(tax);
        prompt.push_str("\n</taxonomy>\n\n");
    }

    if let Some(sty) = style {
        prompt.push_str("<style_guide>\n");
        prompt.push_str(sty);
        prompt.push_str("\n</style_guide>\n\n");
    }

    if let Some(skill) = domain_skill {
        prompt.push_str("<domain_skill>\n");
        prompt.push_str(skill);
        prompt.push_str("\n</domain_skill>\n\n");
    }

    prompt.push_str("<task>\n");
    prompt.push_str(task);
    prompt.push_str("\n</task>");

    if let Some(ctx) = vault_context {
        prompt.push_str("\n\n");
        prompt.push_str(ctx); // Already formatted as <vault_context>...</vault_context>
    }

    prompt
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_sections_present() {
        let prompt = build_system_prompt(
            Some("zones and routing"),
            Some("voice and templates"),
            Some("scientific models"),
            "tidy these notes",
            Some("<vault_context>\nexisting notes\n</vault_context>"),
        );

        // Verify order: taxonomy → style → skill → task → context
        let tax_pos = prompt.find("<taxonomy>").unwrap();
        let sty_pos = prompt.find("<style_guide>").unwrap();
        let skill_pos = prompt.find("<domain_skill>").unwrap();
        let task_pos = prompt.find("<task>").unwrap();
        let ctx_pos = prompt.find("<vault_context>").unwrap();

        assert!(tax_pos < sty_pos);
        assert!(sty_pos < skill_pos);
        assert!(skill_pos < task_pos);
        assert!(task_pos < ctx_pos);
    }

    #[test]
    fn none_sections_omitted() {
        let prompt = build_system_prompt(None, None, None, "just a task", None);

        assert!(!prompt.contains("<taxonomy>"));
        assert!(!prompt.contains("<style_guide>"));
        assert!(!prompt.contains("<domain_skill>"));
        assert!(!prompt.contains("<vault_context>"));
        assert!(prompt.contains("<task>\njust a task\n</task>"));
    }

    #[test]
    fn partial_sections() {
        let prompt = build_system_prompt(
            Some("my taxonomy"),
            None,
            None,
            "do the thing",
            Some("<vault_context>notes</vault_context>"),
        );

        assert!(prompt.contains("<taxonomy>"));
        assert!(!prompt.contains("<style_guide>"));
        assert!(prompt.contains("<task>"));
        assert!(prompt.contains("<vault_context>"));
    }

    #[test]
    fn task_is_always_present() {
        let prompt = build_system_prompt(None, None, None, "", None);
        assert!(prompt.contains("<task>\n\n</task>"));
    }
}
