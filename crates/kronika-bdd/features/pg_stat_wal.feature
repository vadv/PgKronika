Feature: Collector reads pg_stat_wal
  One row per PG14+ cluster. PG14-17 use 1_007_001; PG18 uses 1_007_002.

  Scenario: matrix clusters seal a single-row pg_stat_wal section
    Given the PostgreSQL matrix is booted
    Then each matrix cluster seals a single-row pg_stat_wal section
