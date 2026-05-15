# Daemon Model

trusty-memory runs as a single long-lived process per machine. Every CLI
invocation and every MCP client talks to the same daemon; isolation is provided
by palaces (named namespaces) rather than by separate processes.

This document is the reference for the daemon's lifecycle, file layout, and
operational behavior.

---

## Service Lifecycle (macOS)

On macOS the daemon is managed by launchd. The `service` subcommand wraps the
relevant `launchctl` calls.

```sh
trusty-memory service install     # write plist, bootstrap, start
trusty-memory service status      # is it running? what address?
trusty-memory service logs        # tail last 50 lines of daemon.log
trusty-memory service uninstall   # bootout and remove plist
```

`service install` does three things:

1. Writes `~/Library/LaunchAgents/com.trusty.trusty-memory.plist` with
   `RunAtLoad: true` and `KeepAlive: true`.
2. Bootstraps the agent into the user's launchd domain
   (`launchctl bootstrap gui/<uid> <plist>`).
3. Returns when the daemon is reachable.

On non-macOS platforms `service` returns an error pointing at systemd; native
systemd unit support is on the roadmap.

---

## Auto-Start Behavior

Every CLI subcommand except `serve`, `service`, `setup`, and `hooks` calls
`ensure_daemon()` before doing any work. The logic is:

1. Check the PID file. If a process with that PID exists and the lock file is
   held, treat it as alive.
2. Otherwise, spawn a detached `trusty-memory serve` process.
3. Wait up to **5 seconds** for the daemon's HTTP address file (or stdio
   readiness signal) to appear.
4. If the timeout expires, the command fails with a startup error.

This means commands like `trusty-memory remember`, `recall`, and `status` will
transparently start the daemon if none is running. You only need an explicit
`service install` or `start` if you want the daemon up before the first CLI
call (for example, so an MCP client doesn't pay the cold-start cost).

`serve`, `service`, `setup`, and `hooks` skip `ensure_daemon` because they
either *are* the daemon or operate on its environment.

---

## Port Discovery

The daemon auto-binds an HTTP/SSE listener starting at `127.0.0.1:3031`. If
that port is taken it walks forward up to 20 ports.

The chosen address is written to:

```
~/Library/Application Support/trusty-memory/http_addr
```

`trusty-memory service status` and `trusty-memory status` read this file to
report the live address. Clients that need to discover the daemon should read
it too — never hard-code 3031.

If the daemon is launched with `--no-http`, no `http_addr` file is written and
the daemon is reachable only via stdio (i.e. as a child of an MCP client).

---

## Data Directory Layout

macOS:

```
~/Library/Application Support/trusty-memory/
├── palaces/
│   └── <palace-id>/
│       ├── identity.txt          # L0 layer
│       ├── index.usearch         # HNSW vector index
│       ├── kg.sqlite             # temporal knowledge graph (WAL)
│       └── meta.json             # palace metadata
├── http_addr                     # current listener address
├── trusty-memory.pid             # daemon PID
└── trusty-memory.lock            # advisory flock
```

Fallback (non-macOS, or when `Application Support` is unavailable):

```
~/.trusty-memory/palaces/...
```

Logs live in a separate tree regardless of platform:

```
~/.trusty-memory/logs/daemon.log
```

---

## Logs

```sh
trusty-memory service logs        # tail last 50 lines (macOS)
tail -f ~/.trusty-memory/logs/daemon.log
```

Log level is controlled by `RUST_LOG`. Defaults to `info`.

```sh
RUST_LOG=debug trusty-memory serve
```

---

## `--no-http` Mode

```sh
trusty-memory serve --no-http --palace my-project
```

Stdio-only. No TCP listener, no `http_addr` file. The daemon still writes its
PID file so that `ensure_daemon` and `stop` work correctly.

Use this when:

- You want the daemon spawned as a child of a single MCP client and torn down
  when that client exits.
- You don't want a TCP socket on the loopback interface.
- You're testing locally and want hermetic cleanup.

In this mode, the daemon exits on stdin EOF.

---

## Shutdown

```sh
trusty-memory stop
```

`stop` reads `trusty-memory.pid` and sends SIGTERM. The daemon then:

1. Stops accepting new HTTP requests.
2. Signals dreamer (background) tasks to wind down.
3. Waits up to **2 seconds** for dreamers to finish their current work.
4. Flushes the vector index and KG to disk.
5. Removes the PID file and `http_addr`.
6. Exits.

If the daemon is running under launchd, `KeepAlive: true` will restart it. Use
`service uninstall` if you want it gone for good.

---

## Troubleshooting

### `trusty-memory doctor`

Runs environment diagnostics: checks the data directory, write permissions,
launchd plist (if present), PID/lock file consistency, and listener
reachability.

```sh
trusty-memory doctor
```

### Stale PID file

Symptom: `ensure_daemon` reports "daemon running" but no process exists.

The advisory `flock` on `trusty-memory.lock` should prevent this, but if a host
is force-rebooted the lock may not be released cleanly. Remove both files
manually and try again:

```sh
rm ~/Library/Application\ Support/trusty-memory/trusty-memory.pid
rm ~/Library/Application\ Support/trusty-memory/trusty-memory.lock
```

### Port already in use

The daemon walks 20 ports up from 3031. If all are taken (extremely unlikely),
`serve` exits with a bind error. Free a port in the 3031–3050 range or run
with `--no-http`.

### "no daemon running" from `status`

Three possibilities:

1. The daemon was launched with `--no-http` and has no `http_addr` file.
   `status` will say "stdio-only".
2. The daemon crashed. Check `~/.trusty-memory/logs/daemon.log`.
3. The daemon was never started. Run `trusty-memory start` or
   `trusty-memory service install`.
