RUST_TOOLCHAIN ?= 1.96.0
TARGET ?= $(shell rustc +$(RUST_TOOLCHAIN) -vV | sed -n 's/^host: //p')
CARGO_BUILD = cargo +$(RUST_TOOLCHAIN) build --locked --target $(TARGET)
SCHEMATHESIS_VERSION ?= 4.24.3
# The reproducible tarball needs GNU tar: --sort=name is not in bsdtar, which is
# what macOS ships as `tar`. `brew install gnu-tar` provides gtar.
TAR ?= $(shell command -v gtar 2>/dev/null || echo tar)

.PHONY: build collector web web-frontend web-frontend-check dump openapi openapi-bundle test-bdd demo-build demo-up demo-down demo-run demo-clean demo-api-smoke

build: ## Build collector, web, and dump for the selected target.
	@$(CARGO_BUILD) -p pg_kronika-collector -p pg_kronika-web -p pg_kronika-dump

collector: ## Build pg_kronika-collector.
	@$(CARGO_BUILD) -p pg_kronika-collector

web: ## Build pg_kronika-web.
	@$(CARGO_BUILD) -p pg_kronika-web

web-frontend: ## Install, build the SPA and pack deterministic static.tar.gz for rust-embed.
	cd web && npm ci && npm run build
	$(TAR) --sort=name --mtime=@0 --owner=0 --group=0 --numeric-owner --exclude='*.map' \
		-czf bins/pg_kronika-web/static.tar.gz -C bins/pg_kronika-web/static .

web-frontend-check: ## Typecheck, lint and test the SPA without building.
	cd web && npm ci && npm run typecheck && npm run lint && npm run test

dump: ## Build pg_kronika-dump.
	@$(CARGO_BUILD) -p pg_kronika-dump

openapi: ## Export the verified multi-file web OpenAPI YAML tree.
	@cargo +$(RUST_TOOLCHAIN) run --locked --target $(TARGET) \
		-p pg_kronika-web --example export_openapi -- \
		bins/pg_kronika-web/openapi

openapi-bundle: ## Export an on-demand single-file OpenAPI bundle under target/.
	@mkdir -p target
	@cargo +$(RUST_TOOLCHAIN) run --locked --target $(TARGET) \
		-p pg_kronika-web --example export_openapi -- \
		--bundle target/pg-kronika-openapi.yaml

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

demo-api-smoke: ## Validate all documented operations against an already running demo.
	@SCHEMATHESIS_VERSION="$(SCHEMATHESIS_VERSION)" python3 scripts/demo-api-smoke.py

demo-clean: ## Wipe demo-data (segments, cluster, report) via the image.
	@scripts/demo-stand.sh clean
