Feature: Collector reads pg_prepared_xacts
  The collector seals two-phase-commit transactions awaiting resolution as a
  per-database aggregate: how many are prepared in each database and the oldest
  one's age, tagged with the database name. An idle cluster prepares none, so
  the section is absent. Stable across PG 10-18.

  Scenario: matrix clusters seal per-database pg_prepared_xacts rows
    Given the PostgreSQL matrix is booted
    Then each matrix cluster seals per-database pg_prepared_xacts rows
