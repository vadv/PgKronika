Feature: Collector reads per-database wraparound ages
  The collector seals one row per database with both wraparound distances:
  age(datfrozenxid) for the transaction-ID axis and mxid_age(datminmxid) for the
  multixact axis. Database names resolve through the segment dictionary. Stable
  across PG 10-18.

  Scenario: matrix clusters seal per-database wraparound ages
    Given the PostgreSQL matrix is booted
    Then each matrix cluster seals per-database wraparound ages
