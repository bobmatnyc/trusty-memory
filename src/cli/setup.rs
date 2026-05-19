//! Setup wizard (`trusty-memory setup`).
//!
//! Why: First-time users should have an obvious, friendly path from `cargo
//! install` to a working chat-with-memory setup. Discovering Claude Code
//! projects, surfacing kuzu data we can migrate, capturing the OpenRouter key,
//! and printing the MCP-server config snippet covers the 80% onboarding.
//! What: An interactive (or non-interactive, default-only) wizard that walks
//! through: data root, project discovery, kuzu migration check, OpenRouter
//! key, and Claude Code MCP hook instructions.
//! Test: Manual; the wizard's individual helpers are exposed and unit-tested
//! where the logic is non-trivial (config save, MCP-config patching).

use crate::cli::config::{default_config_path, UserConfig};
use crate::cli::output::OutputConfig;
use crate::cli::palace::data_root;
use anyhow::{Context, Result};
use colored::Colorize;
use std::io::{BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};
use trusty_memory_core::{Palace, PalaceId, PalaceRegistry};

/// Options forwarded from the `setup` subcommand.
#[derive(Debug, Clone, Copy, Default)]
pub struct SetupOpts {
    pub non_interactive: bool,
    pub skip_migration: bool,
    pub migrate_only: bool,
}

/// Run the setup wizard.
pub async fn handle_setup(opts: SetupOpts, _out: &OutputConfig) -> Result<()> {
    let interactive = !opts.non_interactive && std::io::stdin().is_terminal();

    if opts.migrate_only {
        return run_migration_check(interactive, opts.skip_migration);
    }

    print_welcome();

    // 1. Data root.
    let root = data_root()?;
    println!(
        "{} {}",
        "Data root:".bold(),
        root.display().to_string().cyan()
    );
    if interactive {
        let answer = prompt("Use this directory? [Y/n] ")?;
        if answer.eq_ignore_ascii_case("n") {
            println!("  (Custom data roots aren't supported yet; keeping default.)");
        }
    }
    std::fs::create_dir_all(&root)
        .with_context(|| format!("create data root {}", root.display()))?;
    println!();

    // 2. Discover Claude Code projects.
    let candidates = trusty_common::project_discovery::discover_claude_projects();
    if candidates.is_empty() {
        println!(
            "{}",
            "No Claude Code projects found in standard locations.".dimmed()
        );
    } else {
        println!("{}", "Discovered projects:".bold());
        for (i, p) in candidates.iter().enumerate() {
            println!("  {}. {}", i + 1, p.display());
        }
        if interactive {
            let answer = prompt("Register all as palaces? [y/N] ")?;
            if answer.eq_ignore_ascii_case("y") {
                register_palaces_for(&candidates, &root)?;
            }
        } else {
            println!("  {}", "(non-interactive: skipping registration)".dimmed());
        }
    }
    println!();

    // 3. Kuzu migration check.
    if !opts.skip_migration {
        run_migration_check(interactive, false)?;
        println!();
    }

    // 4. OpenRouter key.
    if interactive {
        let key = prompt("OpenRouter API key (sk-or-...) [enter to skip]: ")?;
        if !key.is_empty() {
            let mut cfg = UserConfig::load().unwrap_or_default();
            cfg.openrouter.api_key = key;
            cfg.save().context("save user config")?;
            println!(
                "{} Saved to {}",
                "✓".green(),
                default_config_path()?.display()
            );
        } else {
            println!(
                "  {}",
                "(skipped — set later with `trusty-memory config set openrouter.api_key ...`)"
                    .dimmed()
            );
        }
    } else {
        println!(
            "{} OpenRouter key (skip in non-interactive mode)",
            "•".dimmed()
        );
    }
    println!();

    // 5. Claude Code MCP hook instructions.
    print_claude_code_hook(interactive)?;
    println!();

    // 6. Completion summary.
    println!("{}", "Setup complete.".bold().green());
    println!("  Data root: {}", root.display());
    println!("  Config:    {}", default_config_path()?.display());
    println!("  Try:       {}", "trusty-memory chat \"hello\"".cyan());

    Ok(())
}

fn print_welcome() {
    println!();
    println!(
        "{} {}",
        "trusty-memory".bold().cyan(),
        format!("v{}", env!("CARGO_PKG_VERSION")).dimmed()
    );
    println!("Machine-wide AI memory service with Memory Palace architecture.");
    println!();
}

fn register_palaces_for(paths: &[PathBuf], data_root: &Path) -> Result<()> {
    for p in paths {
        let Some(name) = p.file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        let palace_id = PalaceId::new(name.to_string());
        let palace = Palace {
            id: palace_id.clone(),
            name: name.to_string(),
            description: Some(format!("Auto-registered from {}", p.display())),
            created_at: chrono::Utc::now(),
            data_dir: data_root.join(name),
        };
        let reg = PalaceRegistry::new();
        match reg.create_palace(data_root, palace) {
            Ok(_) => println!("  {} registered palace `{}`", "✓".green(), name),
            Err(e) => tracing::warn!(palace = %name, error = %e, "skipped registration"),
        }
    }
    Ok(())
}

/// Check the standard kuzu-memory locations and offer to migrate.
///
/// Why: Many existing users have memories trapped in kuzu-memory. Even before
/// the migrator ships, surfacing the path tells them their data is reachable.
/// What: Probes `~/.kuzu/`, `~/.open-mpm/memory/`, `~/.claude-mpm/memory/` and
/// prints a discovery line per match. In interactive mode it asks whether to
/// migrate; the actual migration is delegated to a future
/// `trusty-memory migrate --from kuzu` (placeholder message).
/// Test: Logic is straightforward; covered by manual runs.
pub fn run_migration_check(interactive: bool, skip: bool) -> Result<()> {
    if skip {
        return Ok(());
    }
    let Some(home) = dirs::home_dir() else {
        return Ok(());
    };
    let candidates = [
        home.join(".kuzu"),
        home.join(".open-mpm").join("memory"),
        home.join(".claude-mpm").join("memory"),
    ];
    let mut found = Vec::new();
    for c in &candidates {
        if c.exists() {
            found.push(c.clone());
        }
    }
    if found.is_empty() {
        println!("{}", "No kuzu-memory data found.".dimmed());
        return Ok(());
    }
    println!("{}", "Found kuzu-memory data:".bold());
    for f in &found {
        println!("  {}", f.display());
    }
    if interactive {
        let answer = prompt("Migrate? [y/N] ")?;
        if answer.eq_ignore_ascii_case("y") {
            println!(
                "  {} Migration from kuzu not yet implemented — use \
`trusty-memory migrate --from kuzu <path>` once available.",
                "ℹ".yellow()
            );
        }
    }
    Ok(())
}

fn print_claude_code_hook(interactive: bool) -> Result<()> {
    let Some(home) = dirs::home_dir() else {
        return Ok(());
    };
    let cfg_path = home.join(".claude").join("claude_code_config.json");

    println!("{}", "Claude Code MCP hook".bold());
    println!("  Add to {}:", cfg_path.display());
    println!(
        "{}",
        r#"  {
    "mcpServers": {
      "trusty-memory": {
        "command": "trusty-memory",
        "args": ["serve"]
      }
    }
  }"#
        .cyan()
    );

    if interactive && cfg_path.exists() {
        let answer = prompt("Patch this file automatically? [y/N] ")?;
        if answer.eq_ignore_ascii_case("y") {
            match patch_claude_code_config(&cfg_path) {
                Ok(true) => println!("  {} patched", "✓".green()),
                Ok(false) => println!("  {} already configured", "•".dimmed()),
                Err(e) => println!("  {} patch failed: {e}", "✗".red()),
            }
        }
    }
    Ok(())
}

/// Insert (or update) the `trusty-memory` entry in a Claude Code MCP config
/// file, delegating the JSON upsert and atomic write to `trusty-common`.
///
/// Why: Every trusty-* installer needs to register itself in Claude's
/// `mcpServers` map; sharing `trusty_common::claude_config::patch_mcp_server`
/// keeps the atomic-write + `.bak` behaviour identical across the ecosystem.
/// What: Upserts `mcpServers.trusty-memory` with the standard `serve` entry.
/// Returns `Ok(true)` if a write happened, `Ok(false)` if the entry already
/// matched.
/// Test: `patch_adds_trusty_memory_entry` and `patch_preserves_existing_servers`
/// exercise first-write, idempotency, and preservation of other servers.
pub fn patch_claude_code_config(path: &Path) -> Result<bool> {
    use trusty_common::claude_config::{mcp_server_entry, patch_mcp_server};
    let entry = mcp_server_entry("trusty-memory", &["serve"]);
    patch_mcp_server(path, "trusty-memory", &entry)
}

fn prompt(msg: &str) -> Result<String> {
    print!("{msg}");
    std::io::stdout().flush().ok();
    let mut line = String::new();
    let stdin = std::io::stdin();
    stdin.lock().read_line(&mut line).context("read stdin")?;
    Ok(line.trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn patch_adds_trusty_memory_entry() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("claude_code_config.json");
        std::fs::write(&path, "{}").unwrap();

        let wrote = patch_claude_code_config(&path).unwrap();
        assert!(wrote);

        let raw = std::fs::read_to_string(&path).unwrap();
        let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(v["mcpServers"]["trusty-memory"]["command"], "trusty-memory");

        // Idempotent.
        let again = patch_claude_code_config(&path).unwrap();
        assert!(!again, "second patch should be a no-op");
    }

    #[test]
    fn patch_preserves_existing_servers() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("claude_code_config.json");
        std::fs::write(
            &path,
            r#"{"mcpServers":{"other":{"command":"x","args":[]}}}"#,
        )
        .unwrap();

        let wrote = patch_claude_code_config(&path).unwrap();
        assert!(wrote);

        let raw = std::fs::read_to_string(&path).unwrap();
        let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(v["mcpServers"]["other"]["command"], "x");
        assert_eq!(v["mcpServers"]["trusty-memory"]["command"], "trusty-memory");
    }
}
