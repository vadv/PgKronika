Feature: Collector reads bgwriter and checkpointer stats
  The source-pg collector reads type 1_006_001 from PostgreSQL 15, 16, and 17.
  The scenarios pin the PG15/16 and PG17 catalog layouts.

  Scenario: every version yields a valid bgwriter/checkpointer snapshot
    Given the PostgreSQL matrix is booted
    Then every version reports valid bgwriter/checkpointer stats

  Scenario: every version seals a readable segment with section 1_006_001
    Given the PostgreSQL matrix is booted
    Then every version is collected into a sealed segment with section 1_006_001

  Scenario: kronika tags its queries in pg_stat_activity
    Given the PostgreSQL matrix is booted
    Then kronika queries are tagged in pg_stat_activity
