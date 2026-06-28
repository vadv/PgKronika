Feature: Collector reads pg_stat_wal
  The collector seals the single cluster-wide pg_stat_wal row using the layout
  selected by the PostgreSQL major version. PG 14-17 carry the write/sync
  counters (1_007_001); PG 18 keeps only the generation counters (1_007_002).

  Scenario: matrix clusters seal a single-row pg_stat_wal section
    Given the PostgreSQL matrix is booted
    Then each matrix cluster seals a single-row pg_stat_wal section
