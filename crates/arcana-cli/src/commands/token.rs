//! `arcana token` — print or copy the server's bearer token or OAuth password,
//! for pasting into a client's connection form.

use std::io::Write;
use std::process::{Command, Stdio};

use anyhow::{bail, Context, Result};
use clap::Args;

use super::serve::ServerSettings;
use super::setup::LaunchdEnv;

#[derive(Args)]
pub struct TokenArgs {
    /// The OAuth password (what claude.ai asks for) instead of the bearer token
    #[arg(long)]
    pub password: bool,

    /// Copy to the clipboard instead of printing
    #[arg(long)]
    pub copy: bool,
}

pub fn run_token(args: TokenArgs) -> Result<()> {
    let settings = ServerSettings::load()?;
    // Before `arcana setup` has run, a hand-made launchd job may still hold them.
    let legacy = std::env::var("HOME")
        .ok()
        .map(|h| LaunchdEnv::read(std::path::Path::new(&h)));
    let value = if args.password {
        settings
            .oauth
            .map(|o| o.password)
            .or_else(|| legacy.as_ref().and_then(|l| l.get("ARCANA_OAUTH_PASSWORD")))
    } else {
        settings
            .bearer_token
            .or_else(|| legacy.as_ref().and_then(|l| l.get("ARCANA_BEARER_TOKEN")))
    };
    let what = if args.password {
        "OAuth password"
    } else {
        "bearer token"
    };
    let Some(value) = value else {
        bail!("no {what} configured; run `arcana setup`");
    };
    if args.copy {
        copy(&value).with_context(|| format!("copying the {what}"))?;
        eprintln!("copied the {what} to the clipboard");
    } else {
        println!("{value}");
    }
    Ok(())
}

fn copy(text: &str) -> Result<()> {
    let candidates: &[&[&str]] = &[
        &["pbcopy"],
        &["wl-copy"],
        &["xclip", "-selection", "clipboard"],
    ];
    for cmd in candidates {
        let Ok(mut child) = Command::new(cmd[0])
            .args(&cmd[1..])
            .stdin(Stdio::piped())
            .spawn()
        else {
            continue;
        };
        child
            .stdin
            .take()
            .context("clipboard stdin")?
            .write_all(text.as_bytes())?;
        if child.wait()?.success() {
            return Ok(());
        }
    }
    bail!("no clipboard tool found (pbcopy, wl-copy or xclip); run without --copy")
}
