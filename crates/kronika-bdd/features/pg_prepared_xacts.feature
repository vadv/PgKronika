Feature: Collector reads pg_prepared_xacts
  The collector seals two-phase-commit transactions awaiting resolution as a
  per-database aggregate: how many are prepared in each database and the oldest
  wall-clock and XID ages, tagged with the database name. An idle cluster
  prepares none, so the section is absent. The live matrix covers PG 15-18.

  Scenario: matrix clusters seal prepared pg_prepared_xacts rows
    Given the PostgreSQL matrix is booted
    Then each matrix cluster seals prepared pg_prepared_xacts rows
