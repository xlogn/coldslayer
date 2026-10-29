use anyhow::{Context, Result};
use std::env;
use std::fs;
use std::path::PathBuf;

pub const SKILL_MARKDOWN: &str = include_str!("../skills/coldslayer/SKILL.md");

pub fn install(include_local_workspace: bool) -> Result<Vec<PathBuf>> {
    let home = env::var("HOME")
        .or_else(|_| env::var("USERPROFILE"))
        .context("failed to resolve HOME directory")?;
    let home_path = PathBuf::from(home);

    let mut targets = vec![
        home_path
            .join(".gemini")
            .join("config")
            .join("skills")
            .join("coldslayer")
            .join("SKILL.md"),
        home_path
            .join(".claude")
            .join("skills")
            .join("coldslayer")
            .join("SKILL.md"),
    ];

    if include_local_workspace {
        if let Ok(cwd) = env::current_dir() {
            targets.push(
                cwd.join(".agents")
                    .join("skills")
                    .join("coldslayer")
                    .join("SKILL.md"),
            );
        }
    }

    let mut installed = Vec::new();
    for t in targets {
        if let Some(parent) = t.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&t, SKILL_MARKDOWN)?;
        installed.push(t);
    }
    Ok(installed)
}
