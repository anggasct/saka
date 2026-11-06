# Entry points. The logic lives in `cargo xtask`; on a shared machine every
# target runs in its own cgroup through scripts/limited (skipped when CI=true).

LIMITED := $(if $(CI),,./scripts/limited)

.PHONY: check deps e2e supply-chain test-systemd test-claude-memory test-claude-mcp

check:
	$(LIMITED) cargo xtask check

deps:
	$(LIMITED) cargo xtask deps

# Advisories, licences, bans, sources and unused dependencies; needs network.
supply-chain:
	$(LIMITED) cargo xtask supply-chain

# Needs a user systemd instance; part of `make e2e` (not run in CI).
test-systemd:
	$(LIMITED) cargo xtask test-systemd

# Needs the `claude` CLI on PATH; talks only to a local fake API. Part of `make e2e`.
test-claude-memory:
	$(LIMITED) cargo xtask test-claude-memory

# Needs the `claude` CLI on PATH; talks only to a local fake API and `saka mcp`. Part of `make e2e`.
test-claude-mcp:
	$(LIMITED) cargo xtask test-claude-mcp

# Opt-in: a real loop against a test repository with real credentials (see docs/guide.md).
e2e: test-systemd test-claude-memory test-claude-mcp
	$(LIMITED) cargo xtask e2e
