Feature: Collector reads pg_stat_progress_vacuum
  The section is absent when no VACUUM is active. Captured rows carry database
  identity, the autovacuum/manual flag, the PostgreSQL major's dead-tuple
  columns, and dictionary-backed labels.

  Scenario: matrix clusters accept optional progress-vacuum sections
    Given the PostgreSQL matrix is booted
    Then each matrix cluster accepts optional pg_stat_progress_vacuum sections
