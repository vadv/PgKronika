Feature: Collector reads bgwriter and checkpointer stats
  The source-pg collector queries type 1_006_001 from every PostgreSQL version
  in the matrix. Running it live proves the version dispatch and column names
  match each server's real catalog, catching drift the host unit tests cannot.

  Scenario: every version yields a plausible bgwriter/checkpointer snapshot
    Given the PostgreSQL matrix is booted
    Then every version reports plausible bgwriter/checkpointer stats

  Scenario: every version seals a readable segment with section 1_006_001
    Given the PostgreSQL matrix is booted
    Then every version is collected into a sealed segment with section 1_006_001
