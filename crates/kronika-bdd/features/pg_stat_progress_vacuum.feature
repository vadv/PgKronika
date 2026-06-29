Feature: Collector reads pg_stat_progress_vacuum
  The section is absent when no VACUUM is active. Captured rows use the
  PostgreSQL major's dead-tuple columns and dictionary-backed labels.

  Scenario: matrix clusters accept optional progress-vacuum rows
    Given the PostgreSQL matrix is booted
    Then each matrix cluster accepts optional pg_stat_progress_vacuum rows
