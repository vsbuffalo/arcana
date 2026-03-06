use std::sync::Arc;

use async_trait::async_trait;

use crate::permissions::{ApprovalResult, ToolPermission};
use crate::types::ToolDef;

// ---------------------------------------------------------------------------
// ToolExecutor trait
// ---------------------------------------------------------------------------

#[async_trait]
pub trait ToolExecutor: Send + Sync {
    async fn execute(&self, name: &str, input: &serde_json::Value) -> Result<String, String>;
    fn tool_defs(&self) -> Vec<ToolDef>;
}

// ---------------------------------------------------------------------------
// CompositeExecutor
// ---------------------------------------------------------------------------

/// Dispatches tool calls by name prefix to the appropriate executor.
#[derive(Default)]
pub struct CompositeExecutor {
    entries: Vec<(String, Arc<dyn ToolExecutor>)>,
}

impl CompositeExecutor {
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    /// Register an executor for tools matching the given prefix.
    pub fn with(mut self, prefix: &str, executor: Arc<dyn ToolExecutor>) -> Self {
        self.entries.push((prefix.to_string(), executor));
        self
    }
}

#[async_trait]
impl ToolExecutor for CompositeExecutor {
    async fn execute(&self, name: &str, input: &serde_json::Value) -> Result<String, String> {
        for (prefix, executor) in &self.entries {
            if name.starts_with(prefix.as_str()) {
                return executor.execute(name, input).await;
            }
        }
        Err(format!("unknown tool: {name}"))
    }

    fn tool_defs(&self) -> Vec<ToolDef> {
        self.entries
            .iter()
            .flat_map(|(_, executor)| executor.tool_defs())
            .collect()
    }
}

// ---------------------------------------------------------------------------
// PermissionedExecutor
// ---------------------------------------------------------------------------

type ApprovalCallback<'a> =
    Box<dyn Fn(&str, &str, &serde_json::Value) -> ApprovalResult + Send + Sync + 'a>;

/// Wraps any executor with permission checks and optional approval callbacks.
pub struct PermissionedExecutor<'a, E: ToolExecutor> {
    inner: E,
    permissions_fn: Box<dyn Fn(&str) -> ToolPermission + Send + Sync + 'a>,
    approval_fn: Option<ApprovalCallback<'a>>,
}

impl<'a, E: ToolExecutor> PermissionedExecutor<'a, E> {
    pub fn new(
        inner: E,
        permissions_fn: Box<dyn Fn(&str) -> ToolPermission + Send + Sync + 'a>,
    ) -> Self {
        Self {
            inner,
            permissions_fn,
            approval_fn: None,
        }
    }

    pub fn with_approval(mut self, f: ApprovalCallback<'a>) -> Self {
        self.approval_fn = Some(f);
        self
    }
}

#[async_trait]
impl<E: ToolExecutor> ToolExecutor for PermissionedExecutor<'_, E> {
    async fn execute(&self, name: &str, input: &serde_json::Value) -> Result<String, String> {
        match (self.permissions_fn)(name) {
            ToolPermission::Free => {}
            ToolPermission::RequiresApproval => {
                if let Some(ref approval_fn) = self.approval_fn {
                    let desc = format_tool_description(name, input);
                    match approval_fn(name, &desc, input) {
                        ApprovalResult::Approve => {}
                        ApprovalResult::Reject(reason) => {
                            return Err(format!("tool {name} rejected: {reason}"));
                        }
                        ApprovalResult::Edit(new_input) => {
                            return self.inner.execute(name, &new_input).await;
                        }
                    }
                }
            }
            ToolPermission::Blocked => {
                return Err(format!(
                    "Tool '{name}' is not available in this mode. Use vault_draft instead."
                ));
            }
        }

        self.inner.execute(name, input).await
    }

    fn tool_defs(&self) -> Vec<ToolDef> {
        self.inner
            .tool_defs()
            .into_iter()
            .filter(|def| (self.permissions_fn)(&def.name) != ToolPermission::Blocked)
            .collect()
    }
}

fn format_tool_description(tool_name: &str, input: &serde_json::Value) -> String {
    match tool_name {
        "vault_draft" => {
            let path = input.get("path").and_then(|v| v.as_str()).unwrap_or("?");
            format!("Create draft note: {path}")
        }
        "vault_suggest_edit" => {
            let path = input.get("path").and_then(|v| v.as_str()).unwrap_or("?");
            let reason = input.get("reason").and_then(|v| v.as_str()).unwrap_or("?");
            format!("Suggest edit to {path}: {reason}")
        }
        _ => tool_name.to_string(),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    struct MockExecutor {
        prefix: String,
        defs: Vec<ToolDef>,
    }

    impl MockExecutor {
        fn new(prefix: &str, tool_names: &[&str]) -> Self {
            Self {
                prefix: prefix.to_string(),
                defs: tool_names
                    .iter()
                    .map(|n| ToolDef {
                        name: n.to_string(),
                        description: format!("mock {n}"),
                        input_schema: serde_json::json!({"type": "object"}),
                    })
                    .collect(),
            }
        }
    }

    #[async_trait]
    impl ToolExecutor for MockExecutor {
        async fn execute(&self, name: &str, _input: &serde_json::Value) -> Result<String, String> {
            if name.starts_with(&self.prefix) {
                Ok(format!("executed {name}"))
            } else {
                Err(format!("unknown: {name}"))
            }
        }

        fn tool_defs(&self) -> Vec<ToolDef> {
            self.defs.clone()
        }
    }

    #[tokio::test]
    async fn composite_dispatches_by_prefix() {
        let project = Arc::new(MockExecutor::new("project_", &["project_tree"]));
        let vault = Arc::new(MockExecutor::new("vault_", &["vault_search"]));

        let composite = CompositeExecutor::new()
            .with("project_", project)
            .with("vault_", vault);

        let result = composite
            .execute("project_tree", &serde_json::json!({}))
            .await;
        assert_eq!(result.unwrap(), "executed project_tree");

        let result = composite
            .execute("vault_search", &serde_json::json!({}))
            .await;
        assert_eq!(result.unwrap(), "executed vault_search");
    }

    #[tokio::test]
    async fn composite_unknown_tool_errors() {
        let composite = CompositeExecutor::new().with(
            "project_",
            Arc::new(MockExecutor::new("project_", &["project_tree"])),
        );

        let result = composite
            .execute("unknown_tool", &serde_json::json!({}))
            .await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("unknown tool"));
    }

    #[tokio::test]
    async fn composite_tool_defs_flat_maps() {
        let project = Arc::new(MockExecutor::new(
            "project_",
            &["project_tree", "project_read"],
        ));
        let vault = Arc::new(MockExecutor::new("vault_", &["vault_search"]));

        let composite = CompositeExecutor::new()
            .with("project_", project)
            .with("vault_", vault);

        let defs = composite.tool_defs();
        assert_eq!(defs.len(), 3);
        let names: Vec<&str> = defs.iter().map(|d| d.name.as_str()).collect();
        assert!(names.contains(&"project_tree"));
        assert!(names.contains(&"vault_search"));
    }

    #[tokio::test]
    async fn permissioned_blocks_tools() {
        let inner = MockExecutor::new("vault_", &["vault_search", "vault_create"]);
        let executor = PermissionedExecutor::new(
            inner,
            Box::new(|name| {
                if name == "vault_create" {
                    ToolPermission::Blocked
                } else {
                    ToolPermission::Free
                }
            }),
        );

        let result = executor
            .execute("vault_create", &serde_json::json!({}))
            .await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("not available"));
    }

    #[tokio::test]
    async fn permissioned_approves_free_tools() {
        let inner = MockExecutor::new("vault_", &["vault_search"]);
        let executor = PermissionedExecutor::new(inner, Box::new(|_| ToolPermission::Free));

        let result = executor
            .execute("vault_search", &serde_json::json!({}))
            .await;
        assert_eq!(result.unwrap(), "executed vault_search");
    }

    #[tokio::test]
    async fn permissioned_filters_tool_defs() {
        let inner = MockExecutor::new("vault_", &["vault_search", "vault_create", "vault_draft"]);
        let executor = PermissionedExecutor::new(
            inner,
            Box::new(|name| match name {
                "vault_create" => ToolPermission::Blocked,
                "vault_draft" => ToolPermission::RequiresApproval,
                _ => ToolPermission::Free,
            }),
        );

        let defs = executor.tool_defs();
        let names: Vec<&str> = defs.iter().map(|d| d.name.as_str()).collect();
        assert!(names.contains(&"vault_search"));
        assert!(names.contains(&"vault_draft")); // RequiresApproval is still shown
        assert!(!names.contains(&"vault_create")); // Blocked is filtered out
    }

    #[tokio::test]
    async fn permissioned_approval_callback() {
        let inner = MockExecutor::new("vault_", &["vault_draft"]);
        let executor =
            PermissionedExecutor::new(inner, Box::new(|_| ToolPermission::RequiresApproval))
                .with_approval(Box::new(|_name, _desc, _input| {
                    ApprovalResult::Reject("user said no".into())
                }));

        let result = executor
            .execute("vault_draft", &serde_json::json!({"path": "test.md"}))
            .await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("rejected"));
    }
}
