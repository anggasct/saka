# Entry points. The logic lives in `cargo xtask`; on a shared machine every
# target runs in its own cgroup through scripts/limited (skipped when CI=true).

LIMITED := $(if $(CI),,./scripts/limited)

.PHONY: check deps

check:
	$(LIMITED) cargo xtask check

deps:
	$(LIMITED) cargo xtask deps
