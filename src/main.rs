//! trusty-memory CLI entry point.
//!
//! Why: One binary covers serve, palace admin, git ingest, kuzu compatibility,
//! and ad-hoc remember/recall — mirroring how `kuzu-memory` is used so existing
//! muscle memory transfers.
//! What: Parses the top-level `Cli` and dispatches to per-subcommand handlers
//! in the `cli/` module tree.
//! Test: `cargo test --test integration_tests` plus `--help` and `status`
//! integration tests in `tests/integration/cli_test.rs`.

use anyhow::{Context, Result};
use clap::{CommandFactory, Parser};
use clap_complete::generate;
use fs4::FileExt;
use std::io;
use trusty_memory::cli;
use trusty_memory::cli::output::OutputConfig;
use trusty_memory::cli::palace_resolver::{detect_serve_palace, resolve_palace};
use trusty_memory::cli::{Cli, Commands};

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::from_filename(".env.local").ok();

    let cli = Cli::parse();

    // Init tracing + colour handling via the shared trusty-common helpers.
    trusty_common::init_tracing(cli.verbose);
    trusty_common::maybe_disable_color(cli.no_color);

    let palace = resolve_palace(cli.palace.as_deref());

    let out = OutputConfig {
        json: cli.json,
        quiet: cli.quiet,
        no_color: cli.no_color,
    };

    // For all commands except server lifecycle commands, ensure the daemon
    // is running so the web admin panel and dream cycle are always active.
    if !matches!(
        &cli.command,
        Commands::Serve { .. } | Commands::Service(_) | Commands::Setup { .. } | Commands::Hooks(_)
    ) {
        ensure_daemon().await;
    }

    match cli.command {
        Commands::Remember {
            text,
            room,
            tags,
            importance,
        } => {
            cli::handle_remember(&palace, text, room, tags, importance, &out).await?;
        }

        Commands::Recall {
            query,
            top_k,
            room,
            deep,
            decay: _,
            all_palaces,
        } => {
            if all_palaces {
                cli::handle_recall_all(query, top_k, deep, &out).await?;
            } else {
                cli::handle_recall(&palace, query, top_k, room, deep, &out).await?;
            }
        }

        Commands::Forget { id } => {
            cli::handle_forget(&palace, &id, &out).await?;
        }

        Commands::List { limit, room, sort } => {
            cli::handle_list(&palace, limit, room, sort, &out).await?;
        }

        Commands::Palace(sub) => cli::palace::handle(sub, &palace, &out).await?,
        Commands::Kg(sub) => cli::kg::handle(sub, &palace, &out).await?,
        Commands::Git(sub) => cli::git::handle(sub, &palace, &out).await?,
        Commands::Kuzu(sub) => cli::kuzu::handle(sub, &palace, &out).await?,
        Commands::Analytics(sub) => cli::analytics::handle(sub, &palace, &out).await?,
        Commands::Decay(sub) => cli::decay::handle(sub, &palace, &out).await?,
        Commands::Dream(sub) => cli::dream::handle(sub, &palace, &out).await?,

        Commands::Serve {
            http,
            mcp: _,
            palace: serve_palace,
        } => {
            // Auto-detect the default palace from the working directory when
            // `--palace` is omitted (issue #61). A single user-level
            // `~/.claude.json` entry then works across every project without
            // per-project `.mcp.json` overrides.
            let auto_detected = serve_palace.is_none();
            let default_palace = resolve_palace_for_serve(serve_palace.as_deref());
            if auto_detected {
                if let Some(name) = default_palace.as_deref() {
                    eprintln!("info: auto-detected palace '{name}' from working directory");
                }
            }

            tracing::info!(?http, ?default_palace, "Starting trusty-memory MCP server");
            let data_root_for_state = cli::palace::data_root()?;

            // Single-instance file lock (issue #56).
            //
            // Why: `bind_with_auto_port` silently walks forward to the next free
            // port, so two `serve` invocations launched in quick succession both
            // succeed — one on 3031, the next on 3032 — and the daemon count
            // explodes. The discovery file / port probe in `ensure_daemon` has
            // a race window between "does daemon exist?" and "spawn child".
            // An OS-level advisory `flock` collapses that race: only one
            // process can hold the exclusive lock at a time, and the kernel
            // releases it on process death (no stale-lock cleanup needed).
            // What: Open/create `<data_root>/trusty-memory.lock`, request
            // `try_lock_exclusive`. On failure print a clear error and exit
            // with status 1. The `_lock_file` binding keeps the handle alive
            // for the lifetime of the process — dropping it would release the
            // lock.
            // Test: `cargo test --workspace` plus manual smoke (two `serve`
            // invocations: the second exits 1 with the diagnostic).
            // Lock file lives at `<service_root>/trusty-memory.lock` —
            // i.e. the *parent* of the `palaces/` directory, alongside
            // `http_addr` and `trusty-memory.pid`. Issue #56: previously this
            // was placed inside `palaces/` because `cli::palace::data_root()`
            // already appends `/palaces`, hiding the lock where users (and
            // future maintainers) would not look for it. Using
            // `trusty_common::resolve_data_dir` keeps lifecycle artifacts
            // (addr, pid, lock) co-located in the service root.
            let service_root = trusty_common::resolve_data_dir("trusty-memory")
                .context("resolve trusty-memory service root for lock file")?;
            let lock_path = service_root.join("trusty-memory.lock");
            let lock_file = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .open(&lock_path)
                .with_context(|| format!("open lock file at {}", lock_path.display()))?;
            match FileExt::try_lock(&lock_file) {
                Ok(()) => {
                    // Successfully acquired the lock; record our PID so other
                    // probes can tell who holds it.
                    use std::io::Write as _;
                    let mut f = &lock_file;
                    let _ = f.set_len(0);
                    let _ = writeln!(f, "{}", std::process::id());
                }
                Err(_) => {
                    eprintln!(
                        "Another trusty-memory instance is already running. \
                         Use 'trusty-memory stop' to stop it."
                    );
                    std::process::exit(1);
                }
            }
            // Keep the handle alive for the lifetime of the daemon. Dropping
            // it would release the advisory lock.
            let _lock_file = lock_file;

            // Write the PID file unconditionally — *before* the HTTP/stdio
            // branch — so single-instance discovery works for stdio-only
            // daemons too (issue follow-up to #56). Previously this only
            // happened in the HTTP branch, leaving stdio-only daemons
            // invisible to `status`, `stop`, and `doctor`; the result was
            // status output reading `[discovery file missing/stale — found
            // via port scan]` or "not running" depending on whether any
            // *other* trusty-memory HTTP daemon happened to be listening
            // on 3031..=3050. The PID file is the one universal liveness
            // marker that applies to every serve mode.
            //
            // Why: stdio-only mode (the default Claude Code hook path since
            // issue #61) has no HTTP listener to advertise via `http_addr`,
            // but the daemon process is still very much alive. Operators and
            // scripts need *some* discovery artifact regardless of transport.
            // What: Write the current PID to `<service_root>/trusty-memory.pid`
            // here, before any branching. Failure is logged but non-fatal so
            // the daemon still serves even if the data dir is read-only
            // (e.g. a misconfigured sandbox) — clobbering a previous PID
            // file is fine because the file-lock above already guarantees
            // single-instance.
            // Test: `cargo test --workspace`; manual smoke: run
            // `trusty-memory serve &` and confirm
            // `cat <service_root>/trusty-memory.pid` matches the bg PID
            // and `trusty-memory status` reports the daemon as running.
            if let Err(e) = cli::stop::write_pid_file(std::process::id()) {
                tracing::warn!("could not write daemon pid file: {e:#}");
            }

            // Auto-create the default palace if --palace was supplied and the
            // palace doesn't yet exist on disk.
            //
            // Why: Issue #26 — open-mpm spawns one trusty-memory process per
            // project and binds it to a project-scoped palace; requiring a
            // separate `palace new` step would fight that integration.
            // What: If `default_palace` is set and not already persisted under
            // `data_root`, build a `Palace` record and call `create_palace`
            // (which writes metadata + opens a handle).
            // Test: covered by integration via `default_palace_used_when_arg_omitted`.
            if let Some(name) = default_palace.as_deref() {
                let pid = trusty_memory_core::PalaceId::new(name);
                let palace_dir = data_root_for_state.join(pid.as_str());
                if !palace_dir.join("palace.json").exists() {
                    tracing::info!(palace = %name, "auto-creating default palace");
                    let registry = trusty_memory_core::PalaceRegistry::new();
                    let palace = trusty_memory_core::Palace {
                        id: pid,
                        name: name.to_string(),
                        description: Some(
                            "Auto-created by `trusty-memory serve --palace`".to_string(),
                        ),
                        created_at: chrono::Utc::now(),
                        data_dir: palace_dir,
                    };
                    if let Err(e) = registry.create_palace(&data_root_for_state, palace) {
                        tracing::warn!(palace = %name, "failed to auto-create palace: {e:#}");
                    }
                }
            }

            let state = trusty_memory_mcp::AppState::new(data_root_for_state)
                .with_default_palace(default_palace);

            // Shutdown fan-out for the Dreamer tasks. We create the channel
            // up-front so we can hand the `shutdown_rx` to the background
            // initializer (palaces are opened lazily after HTTP is up).
            let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
            let dream_handles: std::sync::Arc<
                tokio::sync::Mutex<Vec<tokio::task::JoinHandle<()>>>,
            > = std::sync::Arc::new(tokio::sync::Mutex::new(Vec::new()));

            // Spawn Dreamer initialization in the background AFTER the HTTP
            // server binds (below). This avoids blocking startup on opening
            // every palace's SQLite pool — with many (e.g. 71+) palaces the
            // synchronous init pattern exhausts FDs or pool timeouts and
            // crashes the daemon before HTTP can bind (issue #43).
            //
            // Why: Issue #43 — daemon crash-looped under launchd with 71
            // palaces because every palace was opened sequentially before
            // axum bound its socket. The HTTP server must come up first;
            // background consolidation can warm up progressively.
            // What: After HTTP binds, a single background task iterates
            // palaces and yields between opens (50ms sleep) to stagger the
            // SQLite pool creation. Each successfully opened palace spawns
            // its dream loop immediately so consolidation begins ASAP.
            // Test: `cargo test --workspace`; manual smoke via `make deploy`
            // + `curl /health` immediately after launchctl start.
            let mut addr_written = false;
            let serve_result = match http {
                None => {
                    // Default: stdio-only — the primary Claude Code MCP path,
                    // no HTTP listener (issue #61).
                    tokio::select! {
                        r = trusty_memory_mcp::run_stdio(state) => r,
                        _ = tokio::signal::ctrl_c() => {
                            tracing::info!("ctrl-c received, shutting down");
                            Ok(())
                        }
                    }
                }
                Some(http_addr) => {
                    // Opt-in: bind HTTP+SSE *and* serve stdio concurrently.
                    // Port auto-detect: if `http_addr` is taken (or is 0), let the
                    // OS pick / walk forward and discover the actual bound address.
                    let listener = trusty_common::bind_with_auto_port(http_addr, 20).await?;
                    let bound_addr = listener.local_addr()?;

                    // Report the actual address prominently to stdout so users
                    // and scripts can see where the daemon landed.
                    println!(
                        "trusty-memory v{} — HTTP admin panel: http://{}",
                        env!("CARGO_PKG_VERSION"),
                        bound_addr
                    );
                    tracing::info!(%bound_addr, "HTTP server bound");

                    // Write addr to the shared trusty-* discovery location so other
                    // commands and scripts can find the running daemon without a
                    // fixed port. Uses `trusty_common::write_daemon_addr` to keep
                    // the file layout identical to trusty-search.
                    match trusty_common::write_daemon_addr("trusty-memory", &bound_addr.to_string())
                    {
                        Ok(()) => addr_written = true,
                        Err(e) => tracing::warn!("could not write daemon addr file: {e:#}"),
                    }

                    // (PID file is written unconditionally above, before this
                    // branch — covers both HTTP and `--no-http` modes.)

                    // Spawn Dreamer initialization *after* HTTP binds so the
                    // daemon is immediately healthy. Open palaces one-at-a-time
                    // with a small sleep between each to spread SQLite pool
                    // creation and avoid FD exhaustion on hosts with many
                    // palaces (issue #43).
                    let dream_handles_bg = dream_handles.clone();
                    let shutdown_rx_bg = shutdown_rx.clone();
                    tokio::spawn(async move {
                        let root = match cli::palace::data_root() {
                            Ok(r) => r,
                            Err(e) => {
                                tracing::warn!("failed to resolve data root for Dreamer: {e:#}");
                                return;
                            }
                        };
                        let palaces = match trusty_memory_core::PalaceRegistry::list_palaces(&root)
                        {
                            Ok(ps) => ps,
                            Err(e) => {
                                tracing::warn!("failed to enumerate palaces for Dreamer: {e:#}");
                                return;
                            }
                        };
                        // Cap eager palace loading to keep startup footprint
                        // bounded (issue #57). Each opened palace allocates a
                        // SQLite pool + a usearch index handle + a Dreamer task;
                        // on hosts with dozens of palaces this multiplied the
                        // daemon's RSS into the multi-GB range. Palaces not
                        // opened eagerly will still open lazily on first use
                        // via `PalaceRegistry::open_palace`.
                        let max_eager: usize = std::env::var("TRUSTY_MAX_STARTUP_PALACES")
                            .ok()
                            .and_then(|v| v.parse::<usize>().ok())
                            .unwrap_or(5);
                        let total = palaces.len();
                        let eager_count = palaces.len().min(max_eager);
                        tracing::info!(
                            count = total,
                            eager = eager_count,
                            max_eager,
                            "Dreamer background init starting (capped eager open)"
                        );
                        let registry = trusty_memory_core::PalaceRegistry::new();
                        let mut opened = 0usize;
                        for p in palaces.into_iter().take(max_eager) {
                            match registry.open_palace(&root, &p.id) {
                                Ok(handle) => {
                                    let dreamer = std::sync::Arc::new(
                                        trusty_memory_core::dream::Dreamer::new(
                                            trusty_memory_core::dream::DreamConfig::default(),
                                        ),
                                    );
                                    let jh =
                                        dreamer.start_with_shutdown(handle, shutdown_rx_bg.clone());
                                    dream_handles_bg.lock().await.push(jh);
                                    opened += 1;
                                }
                                Err(e) => tracing::warn!(
                                    palace = %p.id,
                                    "failed to open palace for dreamer: {e:#}"
                                ),
                            }
                            // Stagger SQLite pool creation to avoid FD pressure.
                            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                            tokio::task::yield_now().await;
                        }
                        tracing::info!(
                            opened,
                            total,
                            "Dreamer background init complete — consolidation active"
                        );
                    });

                    // In HTTP mode, stdio is best-effort: spawn it detached so that
                    // stdin EOF (e.g., terminal session ending, launchd's /dev/null) does
                    // not terminate the daemon. The HTTP server owns daemon lifecycle.
                    tokio::spawn(trusty_memory_mcp::run_stdio(state.clone()));
                    tokio::select! {
                        r = trusty_memory_mcp::run_http_on(state, listener) => r,
                        _ = tokio::signal::ctrl_c() => {
                            tracing::info!("ctrl-c received, shutting down");
                            Ok(())
                        }
                    }
                }
            };

            // Signal every dream task to exit and wait briefly for cleanup.
            let _ = shutdown_tx.send(true);
            let handles = {
                let mut guard = dream_handles.lock().await;
                std::mem::take(&mut *guard)
            };
            for jh in handles {
                let _ = tokio::time::timeout(std::time::Duration::from_secs(2), jh).await;
            }

            // Remove addr + pid files so stale state doesn't mislead callers
            // after shutdown. Best-effort — not fatal if files are already gone.
            if addr_written {
                if let Ok(dir) = trusty_common::resolve_data_dir("trusty-memory") {
                    let _ = std::fs::remove_file(dir.join("http_addr"));
                }
            }
            let _ = cli::stop::remove_pid_file();

            serve_result?;
        }

        Commands::Service(sub) => {
            cli::service::handle(sub)?;
        }

        Commands::Setup {
            non_interactive,
            skip_migration,
            migrate_only,
        } => {
            let opts = cli::setup::SetupOpts {
                non_interactive,
                skip_migration,
                migrate_only,
            };
            cli::setup::handle_setup(opts, &out).await?;
        }

        Commands::Chat {
            message,
            top_k,
            remember,
        } => {
            let opts = cli::chat::ChatOpts {
                message,
                remember,
                top_k,
            };
            cli::chat::handle_chat(&palace, opts, &out).await?;
        }

        Commands::Config(sub) => match sub {
            cli::ConfigCommands::Show => {
                let cfg = cli::config::UserConfig::load()?;
                let path = cli::config::default_config_path()?;
                println!("config: {}", path.display());
                println!("{}", toml::to_string_pretty(&cfg)?);
            }
            cli::ConfigCommands::Set { key, value } => {
                let mut cfg = cli::config::UserConfig::load().unwrap_or_default();
                cfg.set_dotted(&key, &value)?;
                cfg.save()?;
                let path = cli::config::default_config_path()?;
                println!("set {key} in {}", path.display());
            }
        },

        Commands::Convert(args) => {
            cli::convert::handle_convert(args).await?;
        }

        Commands::Bench(sub) => match sub {
            cli::BenchCommands::Compare(args) => {
                let opts = cli::bench::BenchCompareOpts {
                    corpus: args.corpus,
                    top_k: args.top_k,
                    mempalace: args.mempalace,
                    kuzu: args.kuzu,
                    json: args.json,
                };
                cli::bench::handle_bench_compare(opts).await?;
            }
        },

        Commands::Hooks(args) => {
            cli::hooks::handle(args, &palace, &out).await?;
        }

        Commands::Backup(args) => {
            cli::backup::handle_backup(args, &out).await?;
        }

        Commands::Restore(args) => {
            cli::backup::handle_restore(args, &out).await?;
        }

        Commands::Status => {
            let binary = std::env::current_exe()?;
            let root = cli::palace::data_root()?;
            let root_clone = root.clone();
            let palaces = tokio::task::spawn_blocking(move || {
                trusty_memory_core::PalaceRegistry::list_palaces(&root_clone)
            })
            .await??;
            println!("trusty-memory v{}", env!("CARGO_PKG_VERSION"));
            println!("binary: {}", binary.display());
            println!("data_root: {}", root.display());
            println!("palaces: {}", palaces.len());
            println!("active palace: {palace}");

            // Discover the running daemon's HTTP address. Issue #50 — relying
            // solely on the discovery file produced false "not running"
            // reports when the file was missing (e.g. launchd-managed daemons
            // with a different `HOME`). `daemon_probe::probe_daemon` tries
            // the env var, the discovery file, and a candidate port range
            // before giving up.
            // Check the PID file first: it's the authoritative liveness
            // signal regardless of transport (HTTP or `--no-http` stdio).
            // For HTTP daemons we still want to print the bound address, so
            // we do the HTTP probe second and prefer its richer output —
            // but only when the source is more specific than a blind
            // candidate-port scan. Otherwise the PID-file signal wins,
            // which avoids the failure mode where `status` reports a
            // misleading `HTTP: http://127.0.0.1:3031` for a `--no-http`
            // daemon just because some unrelated process answers on 3031.
            let pid_proc = cli::daemon_probe::probe_pid_file();
            match cli::daemon_probe::probe_daemon() {
                Some(found)
                    if !matches!(found.source, cli::daemon_probe::AddrSource::CandidatePort)
                        || pid_proc.is_none() =>
                {
                    let tag = match found.source {
                        cli::daemon_probe::AddrSource::EnvVar => {
                            format!(" [via ${}]", cli::daemon_probe::HTTP_PORT_ENV)
                        }
                        cli::daemon_probe::AddrSource::DiscoveryFile => String::new(),
                        cli::daemon_probe::AddrSource::CandidatePort => {
                            " [discovery file missing/stale — found via port scan]".to_string()
                        }
                    };
                    println!("HTTP: http://{}{tag}", found.addr);
                }
                _ => match pid_proc {
                    Some(proc) => println!(
                        "daemon: running (PID {}, stdio-only — no HTTP listener)",
                        proc.pid
                    ),
                    None => println!("daemon: not running (serve not started)"),
                },
            }
        }

        Commands::Dashboard => cli::dashboard::handle(&out).await?,

        Commands::Start { http } => cli::start::handle(http, &out).await?,

        Commands::Stop => cli::stop::handle(&out).await?,

        Commands::Doctor => cli::doctor::handle(&out).await?,

        Commands::Completions { shell } => {
            let mut cmd = Cli::command();
            let name = cmd.get_name().to_string();
            generate(shell, &mut cmd, name, &mut io::stdout());
        }
    }

    Ok(())
}

/// Resolve the default palace for `serve`, auto-detecting from the working
/// directory when `--palace` is omitted.
///
/// Why: Issue #61 — `serve` running as a per-project Claude Code MCP stdio
/// server should not require an explicit `--palace`; a single user-level
/// `~/.claude.json` entry should resolve the correct palace per project.
/// What: Delegates to `detect_serve_palace`, which honours an explicit value,
/// then a `.trusty-memory` marker file, then the cwd directory name — all
/// sanitized to lowercase kebab-case.
/// Test: `palace_resolver::detect_serve_palace_*` unit tests.
fn resolve_palace_for_serve(explicit: Option<&str>) -> Option<String> {
    detect_serve_palace(explicit)
}

/// Ensure the HTTP daemon is running. Spawns it detached if not, waits up to
/// 5 s. Silent on success; prints one warning line on timeout.
///
/// Why: trusty-memory is a server-based system — the daemon (web UI, dream
/// cycle, MCP HTTP) should always be running whenever any CLI command is used.
/// What: Probes `~/.trusty-memory/http_addr`; if the daemon is absent, spawns
/// `trusty-memory serve` with null stdio so it runs independently of the
/// calling terminal, then polls every 200 ms until ready or 5 s elapsed.
/// Test: Covered by manual smoke; unit-testing a background spawn requires
/// process isolation that is out of scope for a unit suite.
async fn ensure_daemon() {
    if daemon_alive() {
        return;
    }
    // Issue #56 — even when the TCP probe says "no daemon", another `serve`
    // process may already be coming up (HTTP not yet bound). Probe the
    // advisory lock file: if we can't take an exclusive lock, somebody else
    // holds it and we must not spawn a duplicate.
    if lock_file_held() {
        // Wait briefly for the in-flight daemon to bind its listener.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while std::time::Instant::now() < deadline {
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            if daemon_alive() {
                return;
            }
        }
        // Either it's still coming up or it crashed without releasing the
        // lock (rare). Either way: don't spawn another instance.
        eprintln!(
            "[warn] another trusty-memory instance appears to hold the daemon lock; \
             not spawning a duplicate"
        );
        return;
    }
    if let Ok(exe) = std::env::current_exe() {
        // Spawn with `--http` (no address — `serve` defaults to 127.0.0.1:3031
        // and auto-increments on conflict). HTTP is opt-in since issue #61, so
        // the auto-spawned background daemon must request it explicitly to
        // keep the web admin panel and discovery file available for CLI use.
        let _ = std::process::Command::new(&exe)
            .arg("serve")
            .arg("--http")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
    }
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while std::time::Instant::now() < deadline {
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        if daemon_alive() {
            return;
        }
    }
    eprintln!("[warn] daemon did not start within 5 s; proceeding without HTTP server");
}

/// Returns true if a trusty-memory daemon is reachable on any known address.
///
/// Why: `ensure_daemon` uses this to decide whether to spawn a fresh `serve`
/// child. Before the issue #50 fix this read only the discovery file, which
/// meant a launchd-managed daemon (different `HOME`, no discovery file in
/// this user's data dir) was treated as dead — causing `ensure_daemon` to
/// spawn a duplicate that fought for the same port.
/// What: Delegates to `cli::daemon_probe::probe_daemon`, which tries the
/// `TRUSTY_MEMORY_HTTP_PORT` env var, then the shared discovery file, then a
/// candidate port range (3031..=3050) on `127.0.0.1`.
/// Test: Exercised by the `status` / `doctor` integration smoke and the
/// `daemon_probe` unit tests.
fn daemon_alive() -> bool {
    // HTTP probe OR a live PID file. The PID-file path matters for
    // `--no-http` daemons (the Claude Code stdio hook), which never bind a
    // TCP listener but are otherwise fully functional. Without this
    // disjunction `ensure_daemon` would treat a healthy stdio daemon as
    // dead and try to spawn a duplicate (which would then fail the
    // single-instance file-lock check and exit 1, leaving no daemon at all
    // for the calling CLI).
    cli::daemon_probe::probe_daemon().is_some() || cli::daemon_probe::probe_pid_file().is_some()
}

/// Returns true when the `trusty-memory.lock` file at the data root is held
/// exclusively by another process.
///
/// Why: Issue #56 — `ensure_daemon` must avoid spawning a duplicate `serve`
/// when another instance is already mid-startup (HTTP not yet bound). The
/// exclusive advisory lock that the live daemon holds gives us a definitive
/// "someone else owns this" signal regardless of discovery-file state.
/// What: Opens the lock file (creating it if missing) and tries a non-
/// blocking exclusive lock. If acquisition fails we treat that as "held by
/// another process" and return true. On success we release immediately so
/// the calling process doesn't accidentally keep the lock.
/// Test: Covered indirectly by the start/stop integration smoke.
fn lock_file_held() -> bool {
    // Must match the lock path used by `Commands::Serve` exactly. The lock
    // sits in the service root (parent of `palaces/`), alongside the addr
    // and pid files, not inside `palaces/` itself (issue #56).
    let Ok(root) = trusty_common::resolve_data_dir("trusty-memory") else {
        return false;
    };
    let path = root.join("trusty-memory.lock");
    let Ok(file) = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)
    else {
        return false;
    };
    match FileExt::try_lock(&file) {
        Ok(()) => {
            // We got the lock — nobody else holds it. Release immediately.
            let _ = FileExt::unlock(&file);
            false
        }
        Err(_) => true,
    }
}
