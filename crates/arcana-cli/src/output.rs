use std::io::IsTerminal;

use colored::Colorize;

/// Check if stderr is a TTY (for commands that output to stderr).
pub fn is_stderr_tty() -> bool {
    std::io::stderr().is_terminal()
}

pub fn print_header(text: &str) {
    println!("{}", text.bold().cyan());
}

pub fn print_kv(key: &str, value: &str) {
    println!("  {}: {}", key.dimmed(), value);
}

pub fn print_separator() {
    println!("{}", "─".repeat(60).dimmed());
}

const RULE_WIDTH: usize = 60;

pub fn print_git_init_info(vault: &arcana_core::Vault) {
    if let Some(info) = vault.git_init_info() {
        if info.newly_created {
            eprintln!("  {} git repository", "initialized".green().bold());
            if info.adopted_files > 0 {
                eprintln!(
                    "  {} {} existing notes as human-authored",
                    "committed".green().bold(),
                    info.adopted_files,
                );
            }
        }
    }
}

pub fn print_search_result(path: &str, snippet: &str) {
    // ──────────── path/to/note.md ────────────
    let label = format!(" {} ", path);
    let label_len = label.len();
    let remaining = RULE_WIDTH.saturating_sub(label_len);
    let left = remaining / 2;
    let right = remaining - left;
    println!(
        "{}{}{}",
        "─".repeat(left).dimmed(),
        label.blue().bold(),
        "─".repeat(right).dimmed()
    );

    if !snippet.is_empty() {
        let clean = snippet
            .replace("<mark>", "\x1b[1;33m")
            .replace("</mark>", "\x1b[0m");
        for line in clean.lines() {
            println!("  {}", line);
        }
    }
    println!();
}
