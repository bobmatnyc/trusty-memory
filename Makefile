# trusty-memory Makefile
# Workflow: ticket -> implement/test -> commit -> patch bump -> deploy -> smoke test

.DEFAULT_GOAL := help
CLOSES ?=

.PHONY: help check test lint fmt build release patch deploy restart smoke all ext-build

help: ## Show this help
	@grep -E '^[a-zA-Z_-]+:.*?## .*$$' $(MAKEFILE_LIST) \
		| awk 'BEGIN {FS = ":.*?## "}; {printf "  \033[36m%-12s\033[0m %s\n", $$1, $$2}'
	@echo ""
	@echo "  Workflow: ticket -> implement/test -> commit -> patch -> deploy -> smoke"

check: ## cargo check --workspace
	cargo check --workspace

test: ## cargo test --workspace
	cargo test --workspace

lint: ## cargo clippy --workspace --all-targets -- -D warnings
	cargo clippy --workspace --all-targets -- -D warnings

fmt: ## cargo fmt --all
	cargo fmt --all

build: ## cargo build (dev)
	cargo build

release: ## cargo build --release
	cargo build --release

patch: ## Bump patch version across workspace, commit, and tag v* (triggers crates.io publish)
	cargo set-version --bump patch
	@NEW_VERSION=$$(grep '^version' Cargo.toml | head -1 | sed 's/version = "\(.*\)"/\1/'); \
	git add Cargo.toml Cargo.lock crates/trusty-memory-core/Cargo.toml crates/trusty-memory-mcp/Cargo.toml; \
	COMMIT_MSG="chore: bump version to v$$NEW_VERSION"; \
	if [ -n "$(CLOSES)" ]; then COMMIT_MSG="$$COMMIT_MSG (closes #$(CLOSES))"; fi; \
	git commit -m "$$COMMIT_MSG"; \
	git tag "v$$NEW_VERSION"; \
	echo "Tagged v$$NEW_VERSION — push with: git push origin main --tags"

deploy: ## Install binary and restart the daemon
	cargo install --path . --locked
	$(MAKE) restart

restart: ## Restart the daemon via launchd (install service first with: trusty-memory service install)
	@echo "Restarting trusty-memory daemon via launchd..."
	@launchctl bootout gui/$$(id -u)/com.bobmatnyc.trusty-memory 2>/dev/null || true
	@launchctl bootout gui/$$(id -u)/com.trusty.trusty-memory 2>/dev/null || true
	@sleep 2
	@if [ -f "$(HOME)/Library/LaunchAgents/com.bobmatnyc.trusty-memory.plist" ]; then \
	    launchctl bootstrap gui/$$(id -u) $(HOME)/Library/LaunchAgents/com.bobmatnyc.trusty-memory.plist; \
	elif [ -f "$(HOME)/Library/LaunchAgents/com.trusty.trusty-memory.plist" ]; then \
	    launchctl bootstrap gui/$$(id -u) $(HOME)/Library/LaunchAgents/com.trusty.trusty-memory.plist; \
	else \
	    echo "ERROR: No launchd plist found. Install with: trusty-memory service install" >&2; \
	    exit 1; \
	fi
	@sleep 3
	@trusty-memory status
	@echo "trusty-memory daemon restarted"

ext-build: ## Build the VS Code / Cursor extension (extensions/cursor)
	cd extensions/cursor && npm install && npm run typecheck && npm run build

smoke: ## Run smoke test (tests/smoke-test.sh)
	bash tests/smoke-test.sh

all: lint test build ## lint + test + build (full pre-commit check)
