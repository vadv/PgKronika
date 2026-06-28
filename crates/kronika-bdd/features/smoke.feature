# Infra smoke test; the collector itself is exercised in collector.feature.
Feature: PostgreSQL matrix smoke
  Every Nix-provided PostgreSQL version boots in parallel and answers a query.
  Collector scenarios use the same cluster setup.

  Scenario: every version is reachable
    Given the PostgreSQL matrix is booted
    Then every version answers a version query
