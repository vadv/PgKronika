Feature: PostgreSQL log-domain stderr fixtures
  The collector reads deterministic stderr fixtures through KRONIKA_LOG_PATH.
  The sealed rows contain grouped bounded facts, never raw line dumps.

  @pg16 @serial
  Scenario: stderr errors are grouped into pg_log_errors
    Given a fresh database on PostgreSQL 16
    And a PostgreSQL stderr log fixture:
      """
      2026-07-05 12:00:00 UTC [1]: ERROR:  relation "a" does not exist
      2026-07-05 12:00:01 UTC [1]: STATEMENT:  select * from a
      2026-07-05 12:00:02 UTC [1]: ERROR:  relation "b" does not exist
      """
    When the collector snapshots the segment
    Then section 1_022_001 has a row with pattern = relation "..." does not exist:
      | severity  | 0               |
      | category  | 9               |
      | count     | 2               |
      | statement | select * from a |
