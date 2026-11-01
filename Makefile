# Entry points. The logic lives in `cargo xtask`; on a shared machine every
# target runs in its own cgroup through scripts/limited (skipped when CI=true).

LIMITED := $(if $(CI),,./scripts/limited)

.PHONY: check deps test-systemd

check:
	$(LIMITED) cargo xtask check

deps:
	$(LIMITED) cargo xtask deps

# Needs a user systemd instance; part of `make e2e` (not run in CI).
test-systemd:
	$(LIMITED) cargo xtask test-systemd
