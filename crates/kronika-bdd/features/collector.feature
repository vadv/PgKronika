Feature: Collector reads bgwriter and checkpointer stats
  The source-pg collector reads the bgwriter family (1_006_001 on PG15/16,
  1_006_002 on PG17) and reset context (1_020_001 on PG15, 1_020_002 on PG16+).
  Each major writes the type_id that matches its source schema.

  Scenario: every version yields a valid bgwriter/checkpointer snapshot
    Given the PostgreSQL matrix is booted
    Then every version reports valid bgwriter/checkpointer stats

  Scenario: every version seals a readable segment with its version's sections
    Given the PostgreSQL matrix is booted
    Then every version is collected into a sealed segment with its version's sections
