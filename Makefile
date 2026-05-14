# trusty-memory Makefile
# Workflow: ticket -> implement/test -> commit -> patch bump -> deploy -> smoke test

.DEFAULT_GOAL := help

.PHONY: help check test lint fmt build release patch deploy smoke all

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
	git commit -m "chore: bump version to v$$NEW_VERSION"; \
	git tag "v$$NEW_VERSION"; \
	echo "Tagged v$$NEW_VERSION — push with: git push origin main --tags"

deploy: ## cargo install --path . --locked (installs trusty-memory binary locally)
	cargo install --path . --locked

smoke: ## Run smoke test (tests/smoke-test.sh)
	bash tests/smoke-test.sh

all: lint test build ## lint + test + build (full pre-commit check)
