.PHONY: test-bdd

test-bdd: ## Run tagged BDD scenarios through the Docker/Nix image path. Example: DEBUG=1 make test-bdd TAGS=@pg_log
	@TAGS="$(TAGS)" DEBUG="$(DEBUG)" scripts/test-bdd-local.sh
