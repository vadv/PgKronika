Feature: Collector reads per-database wraparound ages
  One row per pg_database database. Rows carry age(datfrozenxid),
  mxid_age(datminmxid), and a dictionary-backed datname.

  Scenario: matrix clusters seal per-database wraparound ages
    Given the PostgreSQL matrix is booted
    Then each matrix cluster seals per-database wraparound ages
