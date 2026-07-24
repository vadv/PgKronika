.PHONY: test-bdd demo-build demo-up demo-down demo-run demo-clean

test-bdd: ## Run BDD through Docker/Nix. Optional: DEBUG=1 make test-bdd TAGS=@pg_log
	@TAGS="$(TAGS)" DEBUG="$(DEBUG)" scripts/test-bdd-local.sh

demo-build: ## Build the demo-stand image (PG 17 + collector + stand driver).
	@scripts/demo-stand.sh build

demo-up: ## Start the demo stand: PG 17 under load, collector, web viewer.
	@scripts/demo-stand.sh up

demo-down: ## Stop the demo stand; seals segments and writes demo-data/report.json.
	@scripts/demo-stand.sh down

demo-run: ## One-shot bounded run (DEMO_DURATION_MIN); report in demo-data/report.json.
	@scripts/demo-stand.sh run

demo-clean: ## Wipe demo-data (segments, cluster, report) via the image.
	@scripts/demo-stand.sh clean
