.PHONY: test-bdd

test-bdd: ## Run BDD through Docker/Nix. Optional: DEBUG=1 make test-bdd TAGS=@pg_log
	@TAGS="$(TAGS)" DEBUG="$(DEBUG)" scripts/test-bdd-local.sh
