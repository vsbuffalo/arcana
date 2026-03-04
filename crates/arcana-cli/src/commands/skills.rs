use anyhow::Result;
use arcana_core::ArcanaConfig;
use clap::Args;
use colored::Colorize;

#[derive(Args)]
pub struct SkillsArgs {
    /// Show description and tools for each skill
    #[arg(long)]
    pub verbose: bool,
}

pub fn run_skills(args: SkillsArgs, config: ArcanaConfig, json: bool) -> Result<()> {
    let vault_root = &config.vault.path;
    let skills = arcana_core::list_skills(vault_root);

    if json {
        let out: Vec<serde_json::Value> = skills
            .iter()
            .map(|s| {
                serde_json::json!({
                    "name": s.name,
                    "title": s.title,
                    "description": s.description,
                    "tools": s.tools,
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&out)?);
        return Ok(());
    }

    if skills.is_empty() {
        eprintln!("{}: no skills found in .arcana/skills/", "info".dimmed());
        eprintln!(
            "{}",
            "  create .arcana/skills/<name>.md or .arcana/skills/<name>/SKILL.md".dimmed()
        );
        return Ok(());
    }

    for skill in &skills {
        let title = skill.title.as_deref().unwrap_or("");
        if title.is_empty() {
            println!("{}", skill.name.bold());
        } else {
            println!("{}  {}", skill.name.bold(), title.dimmed());
        }

        if args.verbose {
            if let Some(desc) = &skill.description {
                println!("  {}", desc);
            }
            if !skill.tools.is_empty() {
                println!("  tools: {}", skill.tools.join(", ").cyan());
            }
            println!();
        }
    }

    Ok(())
}
