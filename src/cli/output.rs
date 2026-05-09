//! Output formatting helpers shared across subcommand handlers.
//!
//! Why: All subcommands need consistent header/footer/error output and JSON
//! mode toggling. Centralizing this keeps `main.rs` clean and avoids duplicate
//! formatting logic.
//! What: `OutputConfig` captures the global flags (json/quiet/no_color) and
//! exposes print helpers that respect them.
//! Test: Built-in: covered by integration tests that capture stdout.

use colored::Colorize;
use serde_json::Value;

/// Captures the global output flags so handlers can format consistently.
///
/// Note: `no_color` is consumed at startup (in `main`) to disable ANSI globally
/// via the `colored` crate; it is retained on the struct so handlers that want
/// to render their own raw output can branch on it without re-reading clap state.
#[allow(dead_code)]
pub struct OutputConfig {
    pub json: bool,
    pub quiet: bool,
    pub no_color: bool,
}

impl OutputConfig {
    /// Print a section header (suppressed in `--json` or `--quiet` mode).
    pub fn print_header(&self, palace: &str, context: &str) {
        if self.json || self.quiet {
            return;
        }
        let header = format!("● {palace} / {context}");
        println!("{}", header.bold());
        println!("{}", "─".repeat(60).dimmed());
    }

    /// Print a footer summarizing a result count and timing.
    pub fn print_footer(&self, count: usize, layer: &str, ms: u64) {
        if self.json || self.quiet {
            return;
        }
        println!("{}", "─".repeat(60).dimmed());
        println!(
            "{}",
            format!("{count} results · {layer} · {ms} ms").dimmed()
        );
    }

    /// Pretty-print a JSON value to stdout.
    #[allow(dead_code)]
    pub fn print_json(&self, value: &Value) {
        match serde_json::to_string_pretty(value) {
            Ok(s) => println!("{s}"),
            Err(e) => eprintln!("error serializing JSON: {e}"),
        }
    }

    /// Print an error line on stderr with a red prefix.
    pub fn print_error(&self, msg: &str) {
        eprintln!("{} {msg}", "error:".red().bold());
    }

    /// Print a success line (suppressed when `--quiet`).
    pub fn print_success(&self, msg: &str) {
        if !self.quiet {
            println!("{} {msg}", "✓".green());
        }
    }
}
