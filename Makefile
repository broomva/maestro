# Maestro — governance + build make targets.
# The bstack control metalayer is vendored at ./bstack (BRO-1829).
BSTACK := ./bin/bstack

.PHONY: help typecheck lint format check bstack-doctor bstack-check control-audit janitor p0-exit

help: ## Show this help
	@grep -E '^[a-zA-Z_-]+:.*?## .*$$' $(MAKEFILE_LIST) | \
	  awk 'BEGIN {FS=":.*?## "}; {printf "  \033[36m%-16s\033[0m %s\n", $$1, $$2}'

typecheck: ## tsc --noEmit across the workspaces
	bun run typecheck

lint: ## biome check
	bun run lint

format: ## biome format --write
	bun run format

check: ## typecheck + lint
	bun run check

bstack-doctor: ## bstack primitive-contract compliance (reports gaps, never blocks)
	$(BSTACK) doctor

bstack-check: ## bstack doctor (governance smoke gate)
	$(BSTACK) doctor
	# BRO-1793 tightens this to `--strict` once the CI skill-install story is settled.

control-audit: ## Full metalayer compliance audit
	$(BSTACK) doctor

janitor: ## Branch + worktree janitor (dry-run)
	$(BSTACK) doctor --quiet || true

p0-exit: ## Run the ROADMAP P0 exit gate (BRO-1798) + capture evidence
	bash scripts/p0-exit.sh
