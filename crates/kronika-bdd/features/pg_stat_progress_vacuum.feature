Feature: Collector reads pg_stat_progress_vacuum
  The collector seals one row per in-progress VACUUM. The view is empty unless a
  vacuum runs, so the section is often absent; when present, rows carry the major
  version's dead-tuple columns (counts before PG17, bytes from PG17) and the
  other era's columns stay NULL. The typed layout is covered by codec tests.

  Scenario: matrix clusters validate progress-vacuum rows when present
    Given the PostgreSQL matrix is booted
    Then each matrix cluster validates pg_stat_progress_vacuum rows when a vacuum runs
