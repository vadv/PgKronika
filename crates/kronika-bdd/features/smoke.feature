Feature: PostgreSQL matrix smoke
  Every Nix-provided PostgreSQL version boots in parallel and answers a query.
  This proves the test infrastructure before any collector scenarios are added.

  Scenario: every version is reachable
    Given the PostgreSQL matrix is booted
    Then every version answers a version query
