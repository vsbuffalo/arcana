use std::sync::Arc;

use anyhow::Result;
use arcana_agent::{AgentConfig, AgentEvent, ApprovalResult, ChatSession};
use arcana_core::ArcanaConfig;
use clap::Args;
use colored::Colorize;
use rustyline::error::ReadlineError;
use rustyline::DefaultEditor;
use tokio::sync::Mutex;

#[derive(Args)]
pub struct ChatArgs {
    /// LLM provider override (anthropic, openai, ollama)
    #[arg(long)]
    pub provider: Option<String>,

    /// Model override
    #[arg(long)]
    pub model: Option<String>,

    /// Maximum agent iterations per turn
    #[arg(long)]
    pub max_iterations: Option<usize>,
}

pub fn run_chat(args: ChatArgs, config: ArcanaConfig, profile: Option<String>) -> Result<()> {
    if config.ledger.enabled {
        anyhow::bail!(
            "{} writes drafts and notes directly, which a ledger vault does not allow; \
             ask an agent over MCP instead",
            "chat"
        );
    }
    let vault = arcana_core::Vault::open(config.clone())?;
    crate::output::print_git_init_info(&vault);
    vault.index()?;

    let stats = vault.stats()?;

    // Resolve LLM config: base → --profile → --provider/--model (no op default for chat)
    let llm_config = config
        .resolve_llm(
            profile.as_deref(),
            None,
            args.provider.as_deref(),
            args.model.as_deref(),
        )
        .map_err(|e| anyhow::anyhow!("{e}"))?;

    let backend = arcana_agent::create_backend(&llm_config)
        .map_err(|e| anyhow::anyhow!(
            "{e}\n\nhint: set ANTHROPIC_API_KEY, or use --provider ollama --model <name> for local inference"
        ))?;

    let session_id = uuid::Uuid::new_v4().to_string();
    let agent_config = AgentConfig {
        max_iterations: args.max_iterations.unwrap_or(config.agent.max_iterations),
        max_tokens: config.agent.max_tokens as u64,
        wrap_up_message: None,
        ..Default::default()
    };

    let profile = vault.profile().clone();
    let user_prompts = arcana_agent::UserPrompts::load(&config.vault.path);

    eprintln!(
        "{} {} ({})",
        "arcana chat".bold(),
        format!("v{}", env!("CARGO_PKG_VERSION")).dimmed(),
        format!("{}/{}", backend.provider_name(), backend.model_name()).cyan()
    );
    eprintln!(
        "{}",
        format!(
            "vault: {} notes, {} tags, {} links{}",
            stats.total_notes,
            stats.total_tags,
            stats.total_links,
            if profile.is_empty() {
                ""
            } else {
                ", brain profile loaded"
            }
        )
        .dimmed()
    );
    eprintln!("{}", "type /quit to exit, /help for commands".dimmed());
    eprintln!();

    let vault = Arc::new(Mutex::new(vault));
    let mut session = ChatSession::new(
        backend,
        vault,
        session_id,
        agent_config,
        &profile,
        &user_prompts,
    );

    // Set up history file
    let history_path = config.vault.path.join(".arcana").join("chat_history");
    if let Some(parent) = history_path.parent() {
        std::fs::create_dir_all(parent).ok();
    }

    let mut rl = DefaultEditor::new()?;
    rl.load_history(&history_path).ok();

    let rt = tokio::runtime::Runtime::new()?;

    loop {
        let readline = rl.readline(&format!("{} ", ">".green().bold()));
        match readline {
            Ok(line) => {
                let input = line.trim();
                if input.is_empty() {
                    continue;
                }

                rl.add_history_entry(input)?;

                // Handle commands
                if input.starts_with('/') {
                    match input {
                        "/quit" | "/exit" | "/q" => {
                            eprintln!("{}", "goodbye".dimmed());
                            break;
                        }
                        "/help" | "/h" => {
                            eprintln!("{}", "commands:".bold());
                            eprintln!("  /quit    exit the chat");
                            eprintln!("  /help    show this help");
                            eprintln!("  /model   show current model info");
                            eprintln!("  /clear   clear conversation history");
                            continue;
                        }
                        "/model" => {
                            eprintln!(
                                "{}: {}/{}",
                                "model".bold(),
                                session.provider_name().cyan(),
                                session.model_name().cyan()
                            );
                            continue;
                        }
                        "/clear" => {
                            session = ChatSession::new(
                                arcana_agent::create_backend(&llm_config)?,
                                session.vault_ref(),
                                uuid::Uuid::new_v4().to_string(),
                                AgentConfig {
                                    max_iterations: args
                                        .max_iterations
                                        .unwrap_or(config.agent.max_iterations),
                                    max_tokens: config.agent.max_tokens as u64,
                                    wrap_up_message: None,
                                    ..Default::default()
                                },
                                &profile,
                                &user_prompts,
                            );
                            eprintln!("{}", "conversation cleared".dimmed());
                            continue;
                        }
                        _ => {
                            eprintln!(
                                "{}",
                                format!("unknown command: {input}. Type /help for help.").yellow()
                            );
                            continue;
                        }
                    }
                }

                // Send message to LLM
                let input_owned = input.to_string();
                let result = rt.block_on(async {
                    let (event_tx, mut event_rx) = tokio::sync::mpsc::unbounded_channel();

                    let approval_fn =
                        |name: &str, desc: &str, _input: &serde_json::Value| -> ApprovalResult {
                            eprintln!();
                            eprintln!(
                                "{} {} wants to: {}",
                                "approval needed:".yellow().bold(),
                                name.cyan(),
                                desc
                            );
                            eprint!("  {} ", "[y]es / [n]o:".bold());

                            let mut response = String::new();
                            std::io::stdin().read_line(&mut response).ok();
                            let response = response.trim().to_lowercase();

                            match response.as_str() {
                                "y" | "yes" | "" => ApprovalResult::Approve,
                                _ => ApprovalResult::Reject("user declined".to_string()),
                            }
                        };

                    // Spawn event handler
                    let event_handle = tokio::spawn(async move {
                        while let Some(event) = event_rx.recv().await {
                            match event {
                                AgentEvent::ToolStart { name, .. } => {
                                    eprint!("  {} {name}... ", "tool:".dimmed());
                                }
                                AgentEvent::ToolFinish { .. } => {
                                    eprintln!("{}", "done".green());
                                }
                                AgentEvent::TokenWarning { used, budget } => {
                                    eprintln!(
                                        "  {}",
                                        format!("token usage at 50% ({used}/{budget})").yellow()
                                    );
                                }
                                _ => {}
                            }
                        }
                    });

                    let result = session
                        .send(&input_owned, Some(&approval_fn), Some(&event_tx))
                        .await;

                    drop(event_tx);
                    event_handle.await.ok();

                    result
                });

                match result {
                    Ok(response) => {
                        if !response.text.is_empty() {
                            eprintln!();
                            println!("{}", response.text);
                            eprintln!();
                        }
                        if !response.drafts_created.is_empty() {
                            eprintln!(
                                "  {} {}",
                                "drafts:".dimmed(),
                                response.drafts_created.join(", ").cyan()
                            );
                            eprintln!(
                                "  {}",
                                "use 'arcana review' to approve or reject drafts".dimmed()
                            );
                            eprintln!();
                        }
                    }
                    Err(e) => {
                        eprintln!("{}: {e}", "error".red().bold());
                    }
                }
            }
            Err(ReadlineError::Interrupted) => {
                eprintln!("{}", "^C (type /quit to exit)".dimmed());
            }
            Err(ReadlineError::Eof) => {
                eprintln!("{}", "goodbye".dimmed());
                break;
            }
            Err(e) => {
                eprintln!("{}: {e}", "readline error".red());
                break;
            }
        }
    }

    rl.save_history(&history_path).ok();
    Ok(())
}
