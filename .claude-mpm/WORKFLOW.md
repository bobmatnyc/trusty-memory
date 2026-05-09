# trusty-memory Dev Workflow

## Required Workflow Sequence

ticket -> implement/test -> commit -> patch bump -> deploy locally -> smoke test

## Phase Definitions

### Phase 1: Ticket
- Reference the GitHub issue number (e.g. #1, #2, ...)
- The ticket defines acceptance criteria -- implementation is complete when ALL criteria pass

### Phase 2: Implement + Test
- Agent: rust-engineer
- Must run `cargo test --workspace` and `cargo clippy --workspace --all-targets -- -D warnings`
- Must show raw test output before returning

### Phase 3: Commit
- Stage only relevant files (no .env, no secrets)
- Commit message format: `feat|fix|chore|test(<scope>): <description> (closes #N)`

### Phase 4: Patch Bump
- Agent: local-ops
- Run: `make patch`
- Verifies Cargo.toml version incremented and git tag created

### Phase 5: Deploy Locally
- Agent: local-ops
- Run: `make deploy` (cargo install --path . --locked)
- Verify binary installed: `which trusty-memory`

### Phase 6: Smoke Test
- Agent: local-ops or qa
- Run: `make smoke`
- Must pass all checks; WARN is acceptable for daemon-dependent checks

## Skip Rules
- Phase 4 (patch) may be skipped for chore/docs-only changes
- Phase 5+6 may be skipped if the change is library-only (no CLI surface changes)
- Phase 1 is always required -- no work without a ticket reference

## Success Criteria
All phases green -> mark ticket closed on GitHub
