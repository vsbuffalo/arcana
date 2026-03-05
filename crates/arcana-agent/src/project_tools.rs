use std::io;
use std::path::{Path, PathBuf};

use async_trait::async_trait;
use globset::{Glob, GlobMatcher};
use regex::Regex;
use serde::Deserialize;
use walkdir::WalkDir;

use crate::executor::ToolExecutor;
use crate::types::ToolDef;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

const DEFAULT_IGNORE_DIRS: &[&str] = &[
    ".git",
    "node_modules",
    "__pycache__",
    "target",
    ".venv",
    "venv",
    ".tox",
    ".mypy_cache",
    ".pytest_cache",
    "dist",
    "build",
    ".next",
    ".cache",
];

/// Files matching these extensions are treated as ignore-dir patterns too.
const IGNORE_SUFFIXES: &[&str] = &[".egg-info"];

const MAX_READ_SIZE: u64 = 100_000;
const MAX_LIST_RESULTS: usize = 500;
const MAX_SEARCH_RESULTS: usize = 100;

// ---------------------------------------------------------------------------
// ProjectToolExecutor
// ---------------------------------------------------------------------------

/// Sandboxed read-only tools for exploring external project directories.
pub struct ProjectToolExecutor {
    root: PathBuf, // canonicalized project root
}

impl ProjectToolExecutor {
    /// Create a new executor rooted at the given directory.
    /// The root path is canonicalized for consistent sandboxing.
    pub fn new(project_root: &Path) -> io::Result<Self> {
        let root = project_root.canonicalize()?;
        Ok(Self { root })
    }

    /// Approximate file count (respects ignore rules, capped at 10K for speed).
    pub fn file_count(&self) -> usize {
        walkdir::WalkDir::new(&self.root)
            .into_iter()
            .filter_entry(|e| {
                let name = e.file_name().to_string_lossy();
                !DEFAULT_IGNORE_DIRS.iter().any(|d| name == *d)
                    && !IGNORE_SUFFIXES.iter().any(|s| name.ends_with(s))
            })
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().is_file())
            .take(10_000)
            .count()
    }

    /// Tool definitions for use with LLM tool-use.
    pub fn tool_defs() -> Vec<ToolDef> {
        vec![
            ToolDef {
                name: "project_list_files".into(),
                description: "List files in the project directory, optionally filtered by glob pattern.".into(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "glob": {
                            "type": "string",
                            "description": "Optional glob pattern to filter files (e.g. '*.py', 'src/**/*.rs')"
                        },
                        "path": {
                            "type": "string",
                            "description": "Subdirectory to list (relative to project root)"
                        }
                    }
                }),
            },
            ToolDef {
                name: "project_read_file".into(),
                description: "Read a file's contents from the project. Files over 100KB or binary files are rejected.".into(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "path": {
                            "type": "string",
                            "description": "Relative path to the file"
                        }
                    },
                    "required": ["path"]
                }),
            },
            ToolDef {
                name: "project_search".into(),
                description: "Search file contents with a regex pattern. Returns matching lines with file paths and line numbers.".into(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "pattern": {
                            "type": "string",
                            "description": "Regex pattern to search for"
                        },
                        "glob": {
                            "type": "string",
                            "description": "Optional glob to filter which files to search"
                        },
                        "max_results": {
                            "type": "integer",
                            "description": "Maximum results to return (default: 100)"
                        }
                    },
                    "required": ["pattern"]
                }),
            },
            ToolDef {
                name: "project_tree".into(),
                description: "Show a directory tree of the project. Respects ignore patterns and depth limits.".into(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "path": {
                            "type": "string",
                            "description": "Subdirectory to show (relative to project root)"
                        },
                        "depth": {
                            "type": "integer",
                            "description": "Maximum depth (default: 3, max: 8)"
                        }
                    }
                }),
            },
        ]
    }

    /// Execute a tool by name.
    pub fn dispatch(&self, tool_name: &str, input: &serde_json::Value) -> Result<String, String> {
        match tool_name {
            "project_list_files" => self.exec_list_files(input),
            "project_read_file" => self.exec_read_file(input),
            "project_search" => self.exec_search(input),
            "project_tree" => self.exec_tree(input),
            _ => Err(format!("unknown project tool: {tool_name}")),
        }
    }

    // -----------------------------------------------------------------------
    // Sandboxing
    // -----------------------------------------------------------------------

    /// Resolve a relative path safely within the project root.
    /// Rejects path traversal and symlink escapes.
    fn resolve_safe_path(&self, rel_path: &str) -> Result<PathBuf, String> {
        let joined = self.root.join(rel_path);
        let canonical = joined
            .canonicalize()
            .map_err(|_| format!("path not found: {rel_path}"))?;
        if !canonical.starts_with(&self.root) {
            return Err(format!("path escapes project root: {rel_path}"));
        }
        Ok(canonical)
    }

    // -----------------------------------------------------------------------
    // Tool implementations
    // -----------------------------------------------------------------------

    fn exec_list_files(&self, input: &serde_json::Value) -> Result<String, String> {
        #[derive(Deserialize)]
        struct Input {
            #[serde(default)]
            glob: Option<String>,
            #[serde(default)]
            path: Option<String>,
        }
        let input: Input = serde_json::from_value(input.clone()).map_err(|e| e.to_string())?;

        let start_dir = if let Some(ref p) = input.path {
            self.resolve_safe_path(p)?
        } else {
            self.root.clone()
        };

        let matcher = input.glob.as_deref().map(build_glob).transpose()?;

        let mut paths = Vec::new();
        for entry in walk_filtered(&start_dir) {
            if entry.file_type().is_file() {
                let rel = entry
                    .path()
                    .strip_prefix(&self.root)
                    .unwrap_or(entry.path());
                let rel_str = rel.to_string_lossy();
                if let Some(ref m) = matcher {
                    if !m.is_match(rel) {
                        continue;
                    }
                }
                paths.push(rel_str.to_string());
                if paths.len() >= MAX_LIST_RESULTS {
                    break;
                }
            }
        }

        serde_json::to_string_pretty(&paths).map_err(|e| e.to_string())
    }

    fn exec_read_file(&self, input: &serde_json::Value) -> Result<String, String> {
        #[derive(Deserialize)]
        struct Input {
            path: String,
        }
        let input: Input = serde_json::from_value(input.clone()).map_err(|e| e.to_string())?;
        let path = self.resolve_safe_path(&input.path)?;

        // Check size
        let metadata = std::fs::metadata(&path).map_err(|e| e.to_string())?;
        if metadata.len() > MAX_READ_SIZE {
            return Err(format!(
                "file too large: {} bytes (max {})",
                metadata.len(),
                MAX_READ_SIZE
            ));
        }

        let bytes = std::fs::read(&path).map_err(|e| e.to_string())?;

        // Detect binary: check for null bytes in first 8KB
        let check_len = bytes.len().min(8192);
        if bytes[..check_len].contains(&0) {
            return Err("binary file detected".to_string());
        }

        String::from_utf8(bytes).map_err(|_| "file is not valid UTF-8".to_string())
    }

    fn exec_search(&self, input: &serde_json::Value) -> Result<String, String> {
        #[derive(Deserialize)]
        struct Input {
            pattern: String,
            #[serde(default)]
            glob: Option<String>,
            #[serde(default)]
            max_results: Option<usize>,
        }
        let input: Input = serde_json::from_value(input.clone()).map_err(|e| e.to_string())?;

        let re = Regex::new(&input.pattern)
            .map_err(|e| format!("invalid regex '{}': {e}", input.pattern))?;

        let matcher = input.glob.as_deref().map(build_glob).transpose()?;

        let max = input
            .max_results
            .unwrap_or(MAX_SEARCH_RESULTS)
            .min(MAX_SEARCH_RESULTS);
        let mut results: Vec<serde_json::Value> = Vec::new();

        'outer: for entry in walk_filtered(&self.root) {
            if !entry.file_type().is_file() {
                continue;
            }
            let rel = entry
                .path()
                .strip_prefix(&self.root)
                .unwrap_or(entry.path());

            if let Some(ref m) = matcher {
                if !m.is_match(rel) {
                    continue;
                }
            }

            // Skip binary/large files
            if let Ok(meta) = entry.metadata() {
                if meta.len() > MAX_READ_SIZE {
                    continue;
                }
            }

            let content = match std::fs::read_to_string(entry.path()) {
                Ok(c) => c,
                Err(_) => continue,
            };

            for (line_num, line) in content.lines().enumerate() {
                if re.is_match(line) {
                    results.push(serde_json::json!({
                        "path": rel.to_string_lossy(),
                        "line_number": line_num + 1,
                        "line": line,
                    }));
                    if results.len() >= max {
                        break 'outer;
                    }
                }
            }
        }

        serde_json::to_string_pretty(&results).map_err(|e| e.to_string())
    }

    fn exec_tree(&self, input: &serde_json::Value) -> Result<String, String> {
        #[derive(Deserialize)]
        struct Input {
            #[serde(default)]
            path: Option<String>,
            #[serde(default)]
            depth: Option<usize>,
        }
        let input: Input = serde_json::from_value(input.clone()).map_err(|e| e.to_string())?;

        let start_dir = if let Some(ref p) = input.path {
            self.resolve_safe_path(p)?
        } else {
            self.root.clone()
        };

        let max_depth = input.depth.unwrap_or(3).min(8);
        let mut output = String::new();

        let root_name = start_dir
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or(".");
        output.push_str(root_name);
        output.push('\n');

        build_tree(&start_dir, "", max_depth, 0, &mut output);

        Ok(output)
    }
}

#[async_trait]
impl ToolExecutor for ProjectToolExecutor {
    async fn execute(&self, name: &str, input: &serde_json::Value) -> Result<String, String> {
        self.dispatch(name, input)
    }

    fn tool_defs(&self) -> Vec<ToolDef> {
        ProjectToolExecutor::tool_defs()
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn build_glob(pattern: &str) -> Result<GlobMatcher, String> {
    Glob::new(pattern)
        .map(|g| g.compile_matcher())
        .map_err(|e| format!("invalid glob '{pattern}': {e}"))
}

fn should_ignore(name: &str) -> bool {
    DEFAULT_IGNORE_DIRS.contains(&name) || IGNORE_SUFFIXES.iter().any(|s| name.ends_with(s))
}

/// Walk directory with default ignore patterns.
fn walk_filtered(root: &Path) -> impl Iterator<Item = walkdir::DirEntry> {
    WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| {
            if e.file_type().is_dir() {
                if let Some(name) = e.file_name().to_str() {
                    return !should_ignore(name);
                }
            }
            true
        })
        .filter_map(|e| e.ok())
}

fn build_tree(
    dir: &Path,
    prefix: &str,
    max_depth: usize,
    current_depth: usize,
    output: &mut String,
) {
    if current_depth >= max_depth {
        return;
    }

    let mut entries: Vec<_> = match std::fs::read_dir(dir) {
        Ok(rd) => rd
            .filter_map(|e| e.ok())
            .filter(|e| {
                if let Some(name) = e.file_name().to_str() {
                    !should_ignore(name)
                } else {
                    false
                }
            })
            .collect(),
        Err(_) => return,
    };

    entries.sort_by(|a, b| {
        let a_dir = a.path().is_dir();
        let b_dir = b.path().is_dir();
        match (a_dir, b_dir) {
            (true, false) => std::cmp::Ordering::Less,
            (false, true) => std::cmp::Ordering::Greater,
            _ => a.file_name().cmp(&b.file_name()),
        }
    });

    let total = entries.len();
    for (i, entry) in entries.iter().enumerate() {
        let is_last = i == total - 1;
        let connector = if is_last {
            "\u{2514}\u{2500}\u{2500} "
        } else {
            "\u{251c}\u{2500}\u{2500} "
        };
        let name = entry.file_name().to_string_lossy().to_string();
        let is_dir = entry.path().is_dir();

        output.push_str(prefix);
        output.push_str(connector);
        output.push_str(&name);
        if is_dir {
            output.push('/');
        }
        output.push('\n');

        if is_dir {
            let child_prefix = if is_last {
                format!("{prefix}    ")
            } else {
                format!("{prefix}\u{2502}   ")
            };
            build_tree(
                &entry.path(),
                &child_prefix,
                max_depth,
                current_depth + 1,
                output,
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn setup_project() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();

        // Create file structure
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::create_dir_all(root.join("src/utils")).unwrap();
        std::fs::create_dir_all(root.join("tests")).unwrap();
        std::fs::create_dir_all(root.join("node_modules/pkg")).unwrap();
        std::fs::create_dir_all(root.join(".git/objects")).unwrap();

        std::fs::write(root.join("README.md"), "# Test Project\n").unwrap();
        std::fs::write(
            root.join("src/main.rs"),
            "fn main() {\n    println!(\"hello\");\n}\n",
        )
        .unwrap();
        std::fs::write(root.join("src/lib.rs"), "pub mod utils;\n").unwrap();
        std::fs::write(
            root.join("src/utils/helpers.rs"),
            "pub fn add(a: i32, b: i32) -> i32 {\n    a + b\n}\n",
        )
        .unwrap();
        std::fs::write(
            root.join("tests/test_main.rs"),
            "#[test]\nfn it_works() {\n    assert!(true);\n}\n",
        )
        .unwrap();
        std::fs::write(
            root.join("node_modules/pkg/index.js"),
            "module.exports = {};\n",
        )
        .unwrap();
        std::fs::write(root.join(".git/objects/abc"), "binary").unwrap();

        dir
    }

    // -- Sandboxing --

    #[test]
    fn resolve_normal_path() {
        let dir = setup_project();
        let exec = ProjectToolExecutor::new(dir.path()).unwrap();
        let resolved = exec.resolve_safe_path("src/main.rs").unwrap();
        assert!(resolved.starts_with(&exec.root));
    }

    #[test]
    fn resolve_traversal_blocked() {
        let dir = setup_project();
        let exec = ProjectToolExecutor::new(dir.path()).unwrap();
        let result = exec.resolve_safe_path("../../etc/passwd");
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.contains("not found") || err.contains("escapes"));
    }

    #[cfg(unix)]
    #[test]
    fn resolve_symlink_escape_blocked() {
        let dir = setup_project();
        // Create a symlink pointing outside
        let link_path = dir.path().join("escape");
        std::os::unix::fs::symlink("/tmp", &link_path).unwrap();

        let exec = ProjectToolExecutor::new(dir.path()).unwrap();
        let result = exec.resolve_safe_path("escape");
        assert!(result.is_err());
    }

    // -- List files --

    #[test]
    fn list_files_basic() {
        let dir = setup_project();
        let exec = ProjectToolExecutor::new(dir.path()).unwrap();
        let result = exec
            .dispatch("project_list_files", &serde_json::json!({}))
            .unwrap();
        let paths: Vec<String> = serde_json::from_str(&result).unwrap();
        assert!(paths.contains(&"src/main.rs".to_string()));
        assert!(paths.contains(&"README.md".to_string()));
    }

    #[test]
    fn list_files_glob_filter() {
        let dir = setup_project();
        let exec = ProjectToolExecutor::new(dir.path()).unwrap();
        let result = exec
            .dispatch(
                "project_list_files",
                &serde_json::json!({"glob": "**/*.rs"}),
            )
            .unwrap();
        let paths: Vec<String> = serde_json::from_str(&result).unwrap();
        assert!(paths.iter().all(|p| p.ends_with(".rs")));
        assert!(paths.contains(&"src/main.rs".to_string()));
    }

    #[test]
    fn list_files_ignores_dirs() {
        let dir = setup_project();
        let exec = ProjectToolExecutor::new(dir.path()).unwrap();
        let result = exec
            .dispatch("project_list_files", &serde_json::json!({}))
            .unwrap();
        let paths: Vec<String> = serde_json::from_str(&result).unwrap();
        // Should not include node_modules or .git
        assert!(paths.iter().all(|p| !p.starts_with("node_modules/")));
        assert!(paths.iter().all(|p| !p.starts_with(".git/")));
    }

    // -- Read file --

    #[test]
    fn read_file_normal() {
        let dir = setup_project();
        let exec = ProjectToolExecutor::new(dir.path()).unwrap();
        let result = exec
            .dispatch(
                "project_read_file",
                &serde_json::json!({"path": "src/main.rs"}),
            )
            .unwrap();
        assert!(result.contains("fn main()"));
    }

    #[test]
    fn read_file_too_large() {
        let dir = setup_project();
        // Create a large file
        let big = vec![b'x'; 200_000];
        std::fs::write(dir.path().join("big.bin"), &big).unwrap();

        let exec = ProjectToolExecutor::new(dir.path()).unwrap();
        let result = exec.dispatch("project_read_file", &serde_json::json!({"path": "big.bin"}));
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("too large"));
    }

    #[test]
    fn read_file_binary_detection() {
        let dir = setup_project();
        let mut binary = vec![0u8; 100];
        binary[50] = 0; // null byte
        std::fs::write(dir.path().join("binary.dat"), &binary).unwrap();

        let exec = ProjectToolExecutor::new(dir.path()).unwrap();
        let result = exec.dispatch(
            "project_read_file",
            &serde_json::json!({"path": "binary.dat"}),
        );
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("binary"));
    }

    #[test]
    fn read_file_path_escape() {
        let dir = setup_project();
        let exec = ProjectToolExecutor::new(dir.path()).unwrap();
        let result = exec.dispatch(
            "project_read_file",
            &serde_json::json!({"path": "../../../etc/passwd"}),
        );
        assert!(result.is_err());
    }

    // -- Search --

    #[test]
    fn search_basic() {
        let dir = setup_project();
        let exec = ProjectToolExecutor::new(dir.path()).unwrap();
        let result = exec
            .dispatch("project_search", &serde_json::json!({"pattern": "fn main"}))
            .unwrap();
        let matches: Vec<serde_json::Value> = serde_json::from_str(&result).unwrap();
        assert!(!matches.is_empty());
        assert!(matches[0]["path"].as_str().unwrap().contains("main.rs"));
    }

    #[test]
    fn search_glob_scoped() {
        let dir = setup_project();
        let exec = ProjectToolExecutor::new(dir.path()).unwrap();
        let result = exec
            .dispatch(
                "project_search",
                &serde_json::json!({"pattern": "fn", "glob": "tests/**"}),
            )
            .unwrap();
        let matches: Vec<serde_json::Value> = serde_json::from_str(&result).unwrap();
        assert!(matches
            .iter()
            .all(|m| m["path"].as_str().unwrap().starts_with("tests/")));
    }

    #[test]
    fn search_result_cap() {
        let dir = setup_project();
        let exec = ProjectToolExecutor::new(dir.path()).unwrap();
        let result = exec
            .dispatch(
                "project_search",
                &serde_json::json!({"pattern": ".", "max_results": 2}),
            )
            .unwrap();
        let matches: Vec<serde_json::Value> = serde_json::from_str(&result).unwrap();
        assert!(matches.len() <= 2);
    }

    // -- Tree --

    #[test]
    fn tree_basic() {
        let dir = setup_project();
        let exec = ProjectToolExecutor::new(dir.path()).unwrap();
        let result = exec
            .dispatch("project_tree", &serde_json::json!({}))
            .unwrap();
        assert!(result.contains("src/"));
        assert!(result.contains("main.rs"));
        // Should not contain ignored dirs
        assert!(!result.contains("node_modules"));
        assert!(!result.contains(".git"));
    }

    #[test]
    fn tree_depth_limiting() {
        let dir = setup_project();
        let exec = ProjectToolExecutor::new(dir.path()).unwrap();
        let result = exec
            .dispatch("project_tree", &serde_json::json!({"depth": 1}))
            .unwrap();
        // At depth 1, should show top-level dirs but not their contents
        assert!(result.contains("src/"));
        // Should NOT show files inside src/ at depth 1
        assert!(!result.contains("main.rs"));
    }
}
