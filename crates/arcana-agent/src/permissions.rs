/// Permission level for a tool.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolPermission {
    /// Tool can be called freely without approval.
    Free,
    /// Tool requires user approval before execution.
    RequiresApproval,
    /// Tool is blocked in this mode.
    Blocked,
}

/// Result of an approval request.
#[derive(Debug, Clone)]
pub enum ApprovalResult {
    Approve,
    Reject(String),
    Edit(serde_json::Value),
}

/// Permissions for chat mode: read freely, draft with approval, no direct writes.
pub fn chat_permissions(tool_name: &str) -> ToolPermission {
    match tool_name {
        "vault_search" | "vault_read" | "vault_list" | "vault_stats" => ToolPermission::Free,
        "vault_draft" | "vault_suggest_edit" => ToolPermission::RequiresApproval,
        "vault_create" | "vault_update" => ToolPermission::Blocked,
        _ => ToolPermission::Blocked,
    }
}

/// Permissions for MCP supervised mode: all tools are free (supervised by client).
pub fn mcp_supervised_permissions(_tool_name: &str) -> ToolPermission {
    ToolPermission::Free
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chat_read_tools_are_free() {
        assert_eq!(chat_permissions("vault_search"), ToolPermission::Free);
        assert_eq!(chat_permissions("vault_read"), ToolPermission::Free);
        assert_eq!(chat_permissions("vault_list"), ToolPermission::Free);
        assert_eq!(chat_permissions("vault_stats"), ToolPermission::Free);
    }

    #[test]
    fn chat_draft_tools_need_approval() {
        assert_eq!(
            chat_permissions("vault_draft"),
            ToolPermission::RequiresApproval
        );
        assert_eq!(
            chat_permissions("vault_suggest_edit"),
            ToolPermission::RequiresApproval
        );
    }

    #[test]
    fn chat_write_tools_blocked() {
        assert_eq!(chat_permissions("vault_create"), ToolPermission::Blocked);
        assert_eq!(chat_permissions("vault_update"), ToolPermission::Blocked);
    }

    #[test]
    fn mcp_all_free() {
        assert_eq!(
            mcp_supervised_permissions("vault_create"),
            ToolPermission::Free
        );
        assert_eq!(
            mcp_supervised_permissions("vault_update"),
            ToolPermission::Free
        );
        assert_eq!(
            mcp_supervised_permissions("vault_draft"),
            ToolPermission::Free
        );
    }
}
