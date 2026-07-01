Feature: Collector reads the pg_locks wait tree
  A blocking chain is sealed as a node-centric wait tree: every backend in a
  blocking component gets one row, and the directed edges live in the blocked_by
  list. A waiter points at the backend that blocks it; the blocker is a root with
  an empty blocked_by. An idle cluster has no waits, so the section is absent. The
  live matrix covers PG 14-18 (the PG10-13 layout is covered by a codec golden).

  Scenario: a blocking chain is recorded as a wait tree
    Given the PostgreSQL matrix is booted
    When session H holds a row lock and session W blocks on it
    Then each matrix cluster seals a wait tree with W blocked by H

  Scenario: no lock waits seals no wait-tree section
    Given the PostgreSQL matrix is booted
    Then no matrix cluster seals a pg_locks wait-tree section
