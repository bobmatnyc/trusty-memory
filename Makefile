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

patch: ## Bump patch version in Cargo.toml, commit, tag
	@CURRENT=$$(grep '^version' Cargo.toml | head -1 | sed 's/version = "\(.*\)"/\1/'); \
	MAJOR=$$(echo "$$CURRENT" | cut -d. -f1); \
	MINOR=$$(echo "$$CURRENT" | cut -d. -f2); \
	PATCH=$$(echo "$$CURRENT" | cut -d. -f3); \
	NEW_PATCH=$$((PATCH + 1)); \
	NEW_VERSION="$$MAJOR.$$MINOR.$$NEW_PATCH"; \
	echo "Bumping $$CURRENT -> $$NEW_VERSION"; \
	sed -i.bak "0,/^version = \"$$CURRENT\"/s//version = \"$$NEW_VERSION\"/" Cargo.toml && rm -f Cargo.toml.bak; \
	cargo check --workspace; \
	git add Cargo.toml Cargo.lock; \
	git commit -m "chore: bump version to $$NEW_VERSION"; \
	git tag "v$$NEW_VERSION"; \
	echo "Tagged v$$NEW_VERSION"

deploy: ## cargo install --path . --locked (installs trusty-memory binary locally)
	cargo install --path . --locked

smoke: ## Run smoke test script (scripts/smoke-test.sh)
	bash scripts/smoke-test.sh

all: lint test build ## lint + test + build (full pre-commit check)
