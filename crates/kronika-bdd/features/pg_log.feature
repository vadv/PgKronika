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

  @pg16 @serial
  Scenario: OOM kills and backend crashes are classified in pg_log_errors
    Given a fresh database on PostgreSQL 16
    And a PostgreSQL stderr log fixture:
      """
      2026-07-05 12:01:00 UTC [1]: LOG:  checkpoint starting: immediate force wait
      2026-07-05 12:01:01 UTC [2]: LOG:  server process (PID 4242) was terminated by signal 9: Killed
      2026-07-05 12:01:02 UTC [3]: LOG:  server process (PID 4243) was terminated by signal 11: Segmentation fault
      2026-07-05 12:01:03 UTC [4]: WARNING:  terminating connection because of crash of another server process
      """
    When the collector snapshots the segment
    Then section 1_022_001 has a row with pattern = "server process (...) was terminated by signal ...: Killed":
      | severity | 4 |
      | category | 4 |
      | count    | 1 |
    And section 1_022_001 has a row with pattern = "server process (...) was terminated by signal ...: Segmentation fault":
      | severity | 4 |
      | category | 6 |
      | count    | 1 |
    And section 1_022_001 has a row with pattern = "terminating connection because of crash of another server process":
      | severity | 3 |
      | category | 6 |
      | count    | 1 |

  @pg16 @serial
  Scenario: deadlock DETAIL HINT CONTEXT stay out of statement text
    Given a fresh database on PostgreSQL 16
    And a PostgreSQL stderr log fixture:
      """
      2026-07-05 12:02:00 UTC [1]: ERROR:  deadlock detected
      2026-07-05 12:02:00 UTC [1]: DETAIL:  Process 111 waits for ShareLock on transaction 10; blocked by process 222.
        Process 222 waits for ShareLock on transaction 11; blocked by process 111.
      2026-07-05 12:02:00 UTC [1]: HINT:  See server log for query details.
      2026-07-05 12:02:00 UTC [1]: CONTEXT:  while updating tuple (0,1) in relation "deadlock_probe"
      2026-07-05 12:02:00 UTC [1]: STATEMENT:  UPDATE deadlock_probe SET id = id WHERE id = 1
      """
    When the collector snapshots the segment
    Then section 1_022_001 has a row with pattern = "deadlock detected":
      | severity  | 0                                                                                                                                              |
      | category  | 0                                                                                                                                              |
      | count     | 1                                                                                                                                              |
      | detail    | Process 111 waits for ShareLock on transaction 10; blocked by process 222. Process 222 waits for ShareLock on transaction 11; blocked by process 111. |
      | hint      | See server log for query details.                                                                                                               |
      | context   | while updating tuple (0,1) in relation "deadlock_probe"                                                                                         |
      | statement | UPDATE deadlock_probe SET id = id WHERE id = 1                                                                                                  |
