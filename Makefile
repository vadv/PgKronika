RUST_TOOLCHAIN ?= 1.96.0
TARGET ?= $(shell rustc +$(RUST_TOOLCHAIN) -vV | sed -n 's/^host: //p')
CARGO_BUILD = cargo +$(RUST_TOOLCHAIN) build --locked --target $(TARGET)

.PHONY: build collector web dump swagger test-bdd demo-build demo-up demo-down demo-run demo-clean

build: ## Build collector, web, and dump for the selected target.
	@$(CARGO_BUILD) -p pg_kronika-collector -p pg_kronika-web -p pg_kronika-dump

collector: ## Build pg_kronika-collector.
	@$(CARGO_BUILD) -p pg_kronika-collector

web: ## Build pg_kronika-web.
	@$(CARGO_BUILD) -p pg_kronika-web

dump: ## Build pg_kronika-dump.
	@$(CARGO_BUILD) -p pg_kronika-dump

swagger: ## Export the generated web OpenAPI document to swagger.yaml.
	@cargo +$(RUST_TOOLCHAIN) run --locked --target $(TARGET) \
		-p pg_kronika-web --example export_openapi -- \
		bins/pg_kronika-web/swagger.yaml

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
