Feature: Collector reads instance replication status
  The collector seals one row describing where the instance sits in replication:
  whether it is in recovery, its timeline, the synchronous-replication settings,
  and the primary- or standby-only LSNs. The matrix runs standalone primaries, so
  the standby columns stay NULL; the standby shape is covered by codec tests.

  Scenario: matrix clusters seal their replication instance status
    Given the PostgreSQL matrix is booted
    Then each matrix cluster seals its replication instance status
