Feature: Collector reads pg_stat_archiver
  One pg_stat_archiver row per snapshot. WAL names resolve through the dictionary
  when present.

  Scenario: every version seals a single-row pg_stat_archiver section
    Given the PostgreSQL matrix is booted
    Then every version seals a single-row pg_stat_archiver section
