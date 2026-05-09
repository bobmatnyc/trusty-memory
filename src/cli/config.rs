//! User configuration loaded from `~/.trusty-memory/config.toml`.
//!
//! Why: `chat` and `setup` need a single, shared place to read and write
//! per-user settings (OpenRouter API key, default model, etc.). Centralising
//! the schema and IO here keeps callers free of TOML details and lets us
//! evolve the schema without touching every command.
//! What: Defines `UserConfig` (with an `[openrouter]` table), exposes
//! `UserConfig::load`/`save` and `default_config_path`. Missing files yield
//! defaults rather than errors so first-run code paths stay simple.
//! Test: See `user_config_default_when_missing` and `user_config_roundtrip`.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Default OpenRouter model used when the user hasn't picked one.
pub const DEFAULT_OPENROUTER_MODEL: &str = "anthropic/claude-3-5-sonnet";

/// Default budget in tokens for the memory context block injected into the
/// system prompt. Sized to leave plenty of room for the model's response in a
/// typical 8k–200k context window.
pub const DEFAULT_MAX_CONTEXT_TOKENS: usize = 4096;

/// `[openrouter]` table.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OpenRouterConfig {
    /// API key (`sk-or-...`). Empty when unset.
    #[serde(default)]
    pub api_key: String,
    /// Model slug, e.g. `anthropic/claude-3-5-sonnet`.
    #[serde(default = "default_model")]
    pub model: String,
    /// Memory-context token budget for chat.
    #[serde(default = "default_max_context_tokens")]
    pub max_context_tokens: usize,
    /// Optional system-prompt override. Empty string falls back to a
    /// trusty-memory default at chat time.
    #[serde(default)]
    pub system_prompt: String,
}

fn default_model() -> String {
    DEFAULT_OPENROUTER_MODEL.to_string()
}

fn default_max_context_tokens() -> usize {
    DEFAULT_MAX_CONTEXT_TOKENS
}

impl Default for OpenRouterConfig {
    fn default() -> Self {
        Self {
            api_key: String::new(),
            model: default_model(),
            max_context_tokens: default_max_context_tokens(),
            system_prompt: String::new(),
        }
    }
}

/// Top-level user config persisted at `~/.trusty-memory/config.toml`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct UserConfig {
    #[serde(default)]
    pub openrouter: OpenRouterConfig,
}

impl UserConfig {
    /// Load from the default path.
    ///
    /// Why: First-run flows must succeed even without a config file, so a
    /// missing file returns `UserConfig::default()` rather than an error.
    /// What: Resolves `default_config_path()` and delegates to `load_from`.
    /// Test: `user_config_default_when_missing` exercises the missing-file path.
    pub fn load() -> Result<Self> {
        Self::load_from(&default_config_path()?)
    }

    /// Load from a specific path (used in tests and for `--config` overrides).
    pub fn load_from(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let raw = std::fs::read_to_string(path)
            .with_context(|| format!("read config file {}", path.display()))?;
        let cfg: UserConfig = toml::from_str(&raw)
            .with_context(|| format!("parse config file {}", path.display()))?;
        Ok(cfg)
    }

    /// Save to the default path, creating the parent directory if needed.
    pub fn save(&self) -> Result<()> {
        self.save_to(&default_config_path()?)
    }

    /// Save to a specific path. Creates parent directories.
    pub fn save_to(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create config dir {}", parent.display()))?;
        }
        let toml_str = toml::to_string_pretty(self).context("serialize user config to TOML")?;
        std::fs::write(path, toml_str)
            .with_context(|| format!("write config file {}", path.display()))?;
        Ok(())
    }

    /// Convenience: set a dotted key like `openrouter.api_key`.
    ///
    /// Why: `trusty-memory config set foo.bar value` is a common one-shot.
    /// What: Recognises the schema's known keys; returns `Err` for unknown
    /// keys so we don't silently swallow typos.
    /// Test: Indirectly via `cargo test --workspace` for `chat`/`setup`.
    pub fn set_dotted(&mut self, key: &str, value: &str) -> Result<()> {
        match key {
            "openrouter.api_key" => self.openrouter.api_key = value.to_string(),
            "openrouter.model" => self.openrouter.model = value.to_string(),
            "openrouter.max_context_tokens" => {
                self.openrouter.max_context_tokens = value
                    .parse()
                    .with_context(|| format!("parse usize for {key}"))?;
            }
            "openrouter.system_prompt" => self.openrouter.system_prompt = value.to_string(),
            other => anyhow::bail!("unknown config key: {other}"),
        }
        Ok(())
    }
}

/// Resolve `~/.trusty-memory/config.toml`.
pub fn default_config_path() -> Result<PathBuf> {
    let home = dirs::home_dir().context("could not resolve home directory")?;
    Ok(home.join(".trusty-memory").join("config.toml"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn user_config_default_when_missing() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("nope/config.toml");
        let cfg = UserConfig::load_from(&path).expect("missing file should be ok");
        assert_eq!(cfg, UserConfig::default());
        assert_eq!(cfg.openrouter.model, DEFAULT_OPENROUTER_MODEL);
        assert_eq!(
            cfg.openrouter.max_context_tokens,
            DEFAULT_MAX_CONTEXT_TOKENS
        );
        assert!(cfg.openrouter.api_key.is_empty());
    }

    #[test]
    fn user_config_roundtrip() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let mut cfg = UserConfig::default();
        cfg.openrouter.api_key = "sk-or-test-key".to_string(); // pragma: allowlist secret
        cfg.openrouter.model = "anthropic/claude-3-opus".to_string();
        cfg.openrouter.max_context_tokens = 2048;
        cfg.save_to(&path).unwrap();

        let loaded = UserConfig::load_from(&path).unwrap();
        assert_eq!(loaded, cfg);
        assert_eq!(loaded.openrouter.api_key, "sk-or-test-key"); // pragma: allowlist secret
        assert_eq!(loaded.openrouter.model, "anthropic/claude-3-opus");
        assert_eq!(loaded.openrouter.max_context_tokens, 2048);
    }

    #[test]
    fn set_dotted_known_keys() {
        let mut cfg = UserConfig::default();
        cfg.set_dotted("openrouter.api_key", "sk-or-x").unwrap();
        cfg.set_dotted("openrouter.model", "anthropic/foo").unwrap();
        cfg.set_dotted("openrouter.max_context_tokens", "1234")
            .unwrap();
        assert_eq!(cfg.openrouter.api_key, "sk-or-x");
        assert_eq!(cfg.openrouter.model, "anthropic/foo");
        assert_eq!(cfg.openrouter.max_context_tokens, 1234);
    }

    #[test]
    fn set_dotted_unknown_key_errors() {
        let mut cfg = UserConfig::default();
        let err = cfg.set_dotted("openrouter.unknown", "x").unwrap_err();
        assert!(err.to_string().contains("unknown config key"));
    }
}
