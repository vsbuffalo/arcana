//! `arcana setup` — one command from nothing to a running, connected vault.
//!
//! Each step checks the current state first and does only what is missing,
//! so running setup again is safe; `--dry-run` shows the plan. Steps:
//!
//! 1. vault: create it and enable the attribution ledger if needed
//! 2. global config: point `vault.path` at it
//! 3. server settings: port, public hostnames, OAuth secrets, in
//!    `~/.config/arcana/server.toml` (0600), migrating secrets from an
//!    existing launchd job if there is one
//! 4. background service: launchd (macOS) or systemd --user (Linux), whose
//!    definition holds no secrets
//! 5. Claude Code: register the MCP server
//! 6. check the server answers, then print how to connect everything else

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};
use clap::Args;

use super::serve::{OAuthSettings, ServerSettings};

#[derive(Args)]
pub struct SetupArgs {
    /// Port the background server listens on (loopback only)
    #[arg(long, default_value_t = 8787)]
    pub port: u16,

    /// Public hostname clients reach the server through (a Cloudflare tunnel
    /// or Tailscale name, e.g. notes.example.com); repeatable
    #[arg(long = "public-host")]
    pub public_hosts: Vec<String>,

    /// Do not install or restart the background service
    #[arg(long)]
    pub no_service: bool,

    /// Do not register the MCP server with Claude Code
    #[arg(long)]
    pub no_claude_code: bool,

    /// Show what would change without changing anything
    #[arg(long)]
    pub dry_run: bool,
}

const LABEL: &str = "com.arcana.serve";

struct Plan {
    dry_run: bool,
}

impl Plan {
    /// Report a step. `change` is None when nothing needs doing.
    fn step(&self, what: &str, change: Option<&str>) {
        match change {
            None => eprintln!("  ✓ {what}"),
            Some(c) if self.dry_run => eprintln!("  → {what}: would {c}"),
            Some(c) => eprintln!("  + {what}: {c}"),
        }
    }
}

pub fn run_setup(args: SetupArgs, vault_flag: Option<PathBuf>) -> Result<()> {
    let plan = Plan {
        dry_run: args.dry_run,
    };
    let home = PathBuf::from(std::env::var("HOME").context("HOME is not set")?);
    let exe = std::env::current_exe()?.canonicalize()?;
    let config_path = arcana_core::global_config_path().context("no config directory")?;
    let vault = vault_flag
        .or_else(|| configured_vault(&config_path))
        .unwrap_or_else(|| home.join("vault").join("notes"));
    let vault = if vault.is_relative() {
        std::env::current_dir()?.join(vault)
    } else {
        vault
    };

    eprintln!(
        "arcana setup{}",
        if args.dry_run { " (dry run)" } else { "" }
    );
    eprintln!("  vault: {}", vault.display());
    if let Some(warning) = synced_folder_warning(&home, &vault) {
        eprintln!("  ! {warning}");
    }

    // 1. Vault with the ledger enabled.
    let ledger_on = std::fs::read_to_string(vault.join(".arcana/config.toml"))
        .map(|t| t.contains("enabled = true"))
        .unwrap_or(false);
    plan.step(
        "vault records authorship",
        (!ledger_on).then_some("create the vault and enable the ledger"),
    );
    if !ledger_on && !plan.dry_run {
        let cfg = arcana_core::ArcanaConfig::default().with_vault_path(vault.clone());
        super::ledger::init(&cfg)?;
    }

    // 2. Global config points at the vault.
    let current = configured_vault(&config_path);
    let config_change = match &current {
        Some(p) if same_path(p, &vault) => None,
        Some(_) => Some("change vault.path"),
        None => Some("write vault.path"),
    };
    plan.step(
        &format!("{} → vault", tilde(&home, &config_path)),
        config_change,
    );
    if config_change.is_some() && !plan.dry_run {
        set_vault_path(&config_path, &vault)?;
    }

    // 3. Server settings, with secrets kept out of the service definition.
    let settings_path = ServerSettings::path().context("no config directory")?;
    let mut settings = ServerSettings::load()?;
    let mut changes = Vec::new();
    if settings.port != Some(args.port) {
        settings.port = Some(args.port);
        changes.push("set port");
    }
    for h in &args.public_hosts {
        if !settings.public_hosts.contains(h) {
            settings.public_hosts.push(h.clone());
            changes.push("add public host");
        }
    }
    let old = LaunchdEnv::read(&home);
    if settings.bearer_token.is_none() {
        if let Some(t) = old.get("ARCANA_BEARER_TOKEN") {
            settings.bearer_token = Some(t);
            changes.push("move the bearer token out of the launchd job");
        }
    }
    for h in old
        .get("ARCANA_PUBLIC_HOSTS")
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|h| !h.is_empty())
    {
        if !settings.public_hosts.iter().any(|x| x == h) {
            settings.public_hosts.push(h.to_string());
            changes.push("keep the launchd job's public host");
        }
    }
    if settings.oauth.is_none() {
        settings.oauth = Some(match old.oauth() {
            Some(o) => {
                changes.push("move OAuth secrets out of the launchd job");
                o
            }
            None => {
                changes.push("generate OAuth secrets");
                OAuthSettings {
                    client_id: format!("arcana-{}", random_token(8)),
                    client_secret: random_token(32),
                    password: random_token(16),
                }
            }
        });
    }
    let summary = changes.join(", ");
    plan.step(
        &tilde(&home, &settings_path),
        (!changes.is_empty()).then_some(summary.as_str()),
    );
    if !changes.is_empty() && !plan.dry_run {
        write_private(&settings_path, &render_settings(&settings)?)?;
    }

    // 4. Background service.
    if !args.no_service {
        install_service(&plan, &home, &exe, &vault)?;
    }

    // 5. Claude Code.
    if !args.no_claude_code {
        register_claude_code(&plan, &exe, &vault);
    }

    // 6. Check and explain.
    if !plan.dry_run && !args.no_service {
        match wait_for_server(args.port) {
            true => eprintln!("  ✓ server answering on 127.0.0.1:{}", args.port),
            false => eprintln!(
                "  ! server not answering on 127.0.0.1:{}; see {}",
                args.port,
                log_hint(&home)
            ),
        }
    }
    if git_remote_missing(&vault) {
        eprintln!(
            "  ! the vault has no git remote, so it exists only on this machine; \
             e.g. gh repo create <name> --private --source {} --push",
            vault.display()
        );
    }
    print_next_steps(&settings, &settings_path, &home);
    Ok(())
}

fn configured_vault(config_path: &Path) -> Option<PathBuf> {
    let text = std::fs::read_to_string(config_path).ok()?;
    let cfg: toml::Value = toml::from_str(&text).ok()?;
    let p = cfg.get("vault")?.get("path")?.as_str()?;
    (!p.is_empty()).then(|| PathBuf::from(p))
}

fn same_path(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(x), Ok(y)) => x == y,
        _ => a == b,
    }
}

/// Set `[vault] path`, leaving the rest of the file as it is.
fn set_vault_path(config_path: &Path, vault: &Path) -> Result<()> {
    let existing = std::fs::read_to_string(config_path).unwrap_or_default();
    let line = format!("path = {}", toml_string(&vault.display().to_string()));
    let updated = replace_vault_path(&existing, &line);
    if let Some(dir) = config_path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(config_path, updated)?;
    Ok(())
}

fn replace_vault_path(existing: &str, line: &str) -> String {
    let mut out = Vec::new();
    let mut in_vault = false;
    let mut done = false;
    for l in existing.lines() {
        let t = l.trim();
        if t.starts_with('[') {
            if in_vault && !done {
                out.push(line.to_string());
                done = true;
            }
            in_vault = t == "[vault]";
        }
        if in_vault && !done && t.starts_with("path") && t.contains('=') {
            out.push(line.to_string());
            done = true;
            continue;
        }
        out.push(l.to_string());
    }
    if in_vault && !done {
        out.push(line.to_string());
        done = true;
    }
    if !done {
        if !out.is_empty() {
            out.push(String::new());
        }
        out.push("[vault]".into());
        out.push(line.to_string());
    }
    format!("{}\n", out.join("\n"))
}

fn toml_string(s: &str) -> String {
    toml::Value::String(s.to_string()).to_string()
}

fn render_settings(s: &ServerSettings) -> Result<String> {
    Ok(format!(
        "# Written by `arcana setup`. Holds the server's OAuth secrets: keep it private.\n\
         # The OAuth password is what you type when Claude or ChatGPT asks to connect.\n{}",
        toml::to_string_pretty(s)?
    ))
}

fn write_private(path: &Path, content: &str) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, content)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

/// Hex from the OS random source (via uuid v4, whose bits are random).
fn random_token(bytes: usize) -> String {
    let mut out = String::new();
    while out.len() < bytes * 2 {
        out.push_str(&uuid::Uuid::new_v4().simple().to_string());
    }
    out.truncate(bytes * 2);
    out
}

/// Environment of an existing launchd job, so settings that clients rely on
/// (OAuth secrets, bearer token, public hostnames) survive setup rewriting it.
struct LaunchdEnv {
    plist: PathBuf,
}

impl LaunchdEnv {
    fn read(home: &Path) -> Self {
        LaunchdEnv {
            plist: launchd_plist(home),
        }
    }

    fn get(&self, key: &str) -> Option<String> {
        if !self.plist.exists() {
            return None;
        }
        let out = Command::new("plutil")
            .args([
                "-extract",
                &format!("EnvironmentVariables.{key}"),
                "raw",
                "-o",
                "-",
            ])
            .arg(&self.plist)
            .output()
            .ok()?;
        out.status
            .success()
            .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
            .filter(|v| !v.is_empty())
    }

    fn oauth(&self) -> Option<OAuthSettings> {
        Some(OAuthSettings {
            client_id: self.get("ARCANA_OAUTH_CLIENT_ID")?,
            client_secret: self.get("ARCANA_OAUTH_CLIENT_SECRET")?,
            password: self.get("ARCANA_OAUTH_PASSWORD")?,
        })
    }
}

fn launchd_plist(home: &Path) -> PathBuf {
    home.join("Library/LaunchAgents")
        .join(format!("{LABEL}.plist"))
}

fn systemd_unit(home: &Path) -> PathBuf {
    home.join(".config/systemd/user/arcana-serve.service")
}

fn log_hint(home: &Path) -> String {
    if cfg!(target_os = "macos") {
        tilde(home, &home.join("Library/Logs/arcana-serve.log"))
    } else {
        "journalctl --user -u arcana-serve".into()
    }
}

/// launchd job: runs `arcana serve` at login and keeps it alive. No secrets:
/// the server reads them from server.toml.
fn render_launchd(exe: &Path, vault: &Path, home: &Path) -> String {
    let esc = |p: &Path| xml_escape(&p.display().to_string());
    let logs = home.join("Library/Logs");
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{LABEL}</string>
    <key>ProgramArguments</key>
    <array>
        <string>{exe}</string>
        <string>--vault</string>
        <string>{vault}</string>
        <string>serve</string>
        <string>--transport</string>
        <string>sse</string>
    </array>
    <key>EnvironmentVariables</key>
    <dict>
        <key>RUST_LOG</key>
        <string>info</string>
    </dict>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <true/>
    <key>StandardOutPath</key>
    <string>{out}</string>
    <key>StandardErrorPath</key>
    <string>{err}</string>
</dict>
</plist>
"#,
        exe = esc(exe),
        vault = esc(vault),
        out = esc(&logs.join("arcana-serve.log")),
        err = esc(&logs.join("arcana-serve.err.log")),
    )
}

fn render_systemd(exe: &Path, vault: &Path) -> String {
    format!(
        "[Unit]\nDescription=Arcana notes server\nAfter=network.target\n\n\
         [Service]\nExecStart={} --vault {} serve --transport sse\nRestart=always\nRestartSec=5\n\
         Environment=RUST_LOG=info\n\n[Install]\nWantedBy=default.target\n",
        shell_quote(&exe.display().to_string()),
        shell_quote(&vault.display().to_string()),
    )
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn shell_quote(s: &str) -> String {
    if s.chars()
        .all(|c| c.is_ascii_alphanumeric() || "/._-~".contains(c))
    {
        s.to_string()
    } else {
        format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
    }
}

fn install_service(plan: &Plan, home: &Path, exe: &Path, vault: &Path) -> Result<()> {
    let (path, content) = if cfg!(target_os = "macos") {
        (launchd_plist(home), render_launchd(exe, vault, home))
    } else if cfg!(target_os = "linux") {
        (systemd_unit(home), render_systemd(exe, vault))
    } else {
        eprintln!(
            "  · no background service support on this OS; run `arcana serve --transport sse`"
        );
        return Ok(());
    };
    let existing = std::fs::read_to_string(&path).ok();
    let change = match &existing {
        Some(e) if *e == content => None,
        Some(_) => Some("rewrite (the old one is kept as .bak) and restart"),
        None => Some("install and start"),
    };
    plan.step(
        &format!("background service {}", tilde(home, &path)),
        change,
    );
    if change.is_none() || plan.dry_run {
        return Ok(());
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    if existing.is_some() {
        let mut bak = path.clone().into_os_string();
        bak.push(".bak");
        std::fs::copy(&path, PathBuf::from(bak))?;
    }
    std::fs::write(&path, content)?;
    if cfg!(target_os = "macos") {
        let domain = format!("gui/{}", uid()?);
        let _ = Command::new("launchctl")
            .args(["bootout", &domain])
            .arg(&path)
            .status();
        let ok = Command::new("launchctl")
            .args(["bootstrap", &domain])
            .arg(&path)
            .status()?
            .success();
        if !ok {
            bail!("launchctl bootstrap failed for {}", path.display());
        }
    } else {
        for args in [
            vec!["--user", "daemon-reload"],
            vec!["--user", "enable", "--now", "arcana-serve"],
            vec!["--user", "restart", "arcana-serve"],
        ] {
            if !Command::new("systemctl").args(&args).status()?.success() {
                bail!("systemctl {} failed", args.join(" "));
            }
        }
    }
    Ok(())
}

fn uid() -> Result<String> {
    let out = Command::new("id").arg("-u").output()?;
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn register_claude_code(plan: &Plan, exe: &Path, vault: &Path) {
    let Ok(probe) = Command::new("claude")
        .args(["mcp", "get", "arcana"])
        .output()
    else {
        eprintln!(
            "  · Claude Code not found; later: claude mcp add --scope user arcana -- {} --vault {} serve",
            exe.display(),
            vault.display()
        );
        return;
    };
    if probe.status.success() {
        plan.step("Claude Code MCP server `arcana`", None);
        return;
    }
    plan.step(
        "Claude Code MCP server `arcana`",
        Some("register (user scope)"),
    );
    if plan.dry_run {
        return;
    }
    let status = Command::new("claude")
        .args(["mcp", "add", "--scope", "user", "arcana", "--"])
        .arg(exe)
        .arg("--vault")
        .arg(vault)
        .arg("serve")
        .status();
    if !matches!(status, Ok(s) if s.success()) {
        eprintln!("  ! could not register with Claude Code; run `claude mcp add` yourself");
    }
}

/// Poll the server's discovery endpoint for up to ~5 s.
fn wait_for_server(port: u16) -> bool {
    use std::io::{Read, Write};
    for _ in 0..25 {
        if let Ok(mut s) = std::net::TcpStream::connect(("127.0.0.1", port)) {
            let req =
                "GET /.well-known/oauth-protected-resource HTTP/1.0\r\nHost: localhost\r\n\r\n";
            let mut buf = [0u8; 16];
            if s.write_all(req.as_bytes()).is_ok() && s.read(&mut buf).is_ok() {
                return buf.starts_with(b"HTTP/1.");
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
    false
}

fn git_remote_missing(vault: &Path) -> bool {
    Command::new("git")
        .arg("-C")
        .arg(vault)
        .args(["remote"])
        .output()
        .map(|o| o.status.success() && o.stdout.is_empty())
        .unwrap_or(false)
}

/// Folders that iCloud (or similar) syncs file by file break git and evict
/// files to placeholders; see docs/dev/incidents/2026-10-06-*.
fn synced_folder_warning(home: &Path, vault: &Path) -> Option<String> {
    let synced = [
        "Documents",
        "Desktop",
        "Library/Mobile Documents",
        "Library/CloudStorage",
    ];
    synced
        .iter()
        .any(|d| vault.starts_with(home.join(d)))
        .then(|| {
            "this folder is usually synced by iCloud or a cloud drive, which breaks git \
             and can evict files; prefer a plain folder such as ~/vault/notes"
                .to_string()
        })
}

fn tilde(home: &Path, p: &Path) -> String {
    match p.strip_prefix(home) {
        Ok(rest) => format!("~/{}", rest.display()),
        Err(_) => p.display().to_string(),
    }
}

fn print_next_steps(settings: &ServerSettings, settings_path: &Path, home: &Path) {
    let port = settings.port.unwrap_or(8787);
    eprintln!("\nConnect from other apps:");
    if settings.public_hosts.is_empty() {
        eprintln!(
            "  The server listens only on this machine (127.0.0.1:{port}). To reach it from \
             claude.ai, ChatGPT or your phone, expose it with Tailscale or a Cloudflare tunnel \
             (docs/deployment.md), then rerun: arcana setup --public-host <that hostname>"
        );
        return;
    }
    for h in &settings.public_hosts {
        eprintln!("  MCP URL: https://{h}/mcp");
    }
    eprintln!(
        "  claude.ai: Settings → Connectors → Add custom connector → the URL above.\n  \
         ChatGPT: Settings → Apps → Advanced → Developer mode, then create an app with \
         the URL above and OAuth.\n  When asked to authorize, use the OAuth password in {}.",
        tilde(home, settings_path)
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vault_path_replaced_in_place() {
        let line = r#"path = "/v/new""#;
        assert_eq!(
            replace_vault_path("[vault]\npath = \"/v/old\"\n\n[llm]\nx = 1\n", line),
            "[vault]\npath = \"/v/new\"\n\n[llm]\nx = 1\n"
        );
        assert_eq!(
            replace_vault_path("[llm]\nx = 1\n", line),
            "[llm]\nx = 1\n\n[vault]\npath = \"/v/new\"\n"
        );
        assert_eq!(
            replace_vault_path("[vault]\nexclude = []\n[llm]\n", line),
            "[vault]\nexclude = []\npath = \"/v/new\"\n[llm]\n"
        );
        assert_eq!(replace_vault_path("", line), "[vault]\npath = \"/v/new\"\n");
    }

    #[test]
    fn service_definitions_carry_no_secrets() {
        let exe = Path::new("/Users/me/.cargo/bin/arcana");
        let vault = Path::new("/Users/me/vault/my notes");
        let plist = render_launchd(exe, vault, Path::new("/Users/me"));
        assert!(plist.contains("<string>/Users/me/vault/my notes</string>"));
        assert!(!plist.contains("OAUTH"));
        let unit = render_systemd(exe, vault);
        assert!(unit.contains("--vault \"/Users/me/vault/my notes\" serve"));
        assert!(!unit.contains("OAUTH"));
    }

    #[test]
    fn warns_about_synced_folders() {
        let home = Path::new("/Users/me");
        assert!(synced_folder_warning(home, Path::new("/Users/me/Documents/Obsidian/v")).is_some());
        assert!(synced_folder_warning(home, Path::new("/Users/me/vault/notes")).is_none());
    }

    #[test]
    fn tokens_have_requested_length() {
        assert_eq!(random_token(32).len(), 64);
        assert_ne!(random_token(16), random_token(16));
    }
}
