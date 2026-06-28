Feature: Collector reads pg_stat_archiver
  The source-pg collector reads the single pg_stat_archiver row (type 1_008_001,
  stable across PG 10-18) and seals it. The scenario reads the row back, checks
  the counters, and resolves WAL names through the dictionary when PostgreSQL
  reports them.

  Scenario: every version seals a single-row pg_stat_archiver section
    Given the PostgreSQL matrix is booted
    Then every version seals a single-row pg_stat_archiver section
