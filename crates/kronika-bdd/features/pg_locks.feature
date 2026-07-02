Feature: Collector reads the pg_locks wait tree
  A row-lock wait is sealed as a node-centric wait tree. The waiter row points
  at the holder through blocked_by, and the holder row is the root. The live BDD
  pins the PG14+ layout (1_011_002); codec tests cover the PG10-13 layout.

  @pg17 @lock @serial
  Scenario: row-lock wait is captured as W blocked by H
    Given a fresh database on PostgreSQL 17
    And a database seeded with:
      """
      CREATE TABLE kronika_lock_probe (id int primary key, v int);
      INSERT INTO kronika_lock_probe VALUES (1, 0);
      """
    And session "H" runs and holds its transaction open:
      """
      BEGIN;
      UPDATE kronika_lock_probe SET v = v + 1 WHERE id = 1;
      """
    And session "W" runs and blocks:
      """
      UPDATE kronika_lock_probe SET v = v + 1 WHERE id = 1;
      """
    When the collector snapshots the segment
    Then section 1_011_002 has a row for session "W":
      | pid             | [W]           |
      | blocked_by      | [H]           |
      | depth           | 1             |
      | root_pid        | [H]           |
      | wait_event_type | Lock          |
      | wait_event      | transactionid |
      | lock_locktype   | transactionid |
      | lock_mode       | ShareLock     |
      | lock_granted    | false         |
    And section 1_011_002 has a row for session "H":
      | pid           | [H]  |
      | blocked_by    | []   |
      | depth         | 0    |
      | root_pid      | [H]  |
      | lock_locktype | null |
      | lock_mode     | null |
      | lock_granted  | null |
    And section 1_011_002 blocked_by matches the subset oracle:
      """
      SELECT pg_blocking_pids(pid)
      FROM pg_stat_activity
      WHERE datname = current_database()
        AND wait_event_type = 'Lock'
      """
    And section 1_011_001 is absent from the segment

  @pg17 @lock @serial
  Scenario: no lock waits seals no wait-tree section
    Given a fresh database on PostgreSQL 17
    When the collector snapshots the segment
    Then section 1_011_002 is absent from the segment
    And section 1_011_001 is absent from the segment
