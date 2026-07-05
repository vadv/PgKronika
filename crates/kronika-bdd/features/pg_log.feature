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
  Scenario: deadlock diagnostics are separate from statement text
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

  @pg16 @serial
  Scenario: checkpoint LOG records are typed without becoming errors
    Given a fresh database on PostgreSQL 16
    And a PostgreSQL stderr log fixture:
      """
      2026-07-05 12:03:00 UTC [1]: LOG:  checkpoint starting: time
      2026-07-05 12:03:01 UTC [1]: LOG:  checkpoint complete: wrote 128 buffers (0.2%); 0 WAL file(s) added, 1 removed, 2 recycled; write=1.234 s, sync=0.056 s, total=1.500 s; sync files=7, longest=0.040 s, average=0.008 s; distance=4096 kB, estimate=8192 kB
      2026-07-05 12:03:02 UTC [1]: LOG:  checkpoints are occurring too frequently (3 seconds apart)
      """
    When the collector snapshots the segment
    Then section 1_022_001 is absent from the segment
    And section 1_024_001 has a row with phase = 0:
      | reason | time |
    And section 1_024_001 has a row with phase = 1:
      | buffers_written | 128    |
      | write_ms        | 1234.0 |
      | sync_ms         | 56.0   |
      | total_ms        | 1500.0 |
      | wal_added       | 0      |
      | wal_removed     | 1      |
      | wal_recycled    | 2      |
    And section 1_024_001 has a row with phase = 2:
      | seconds_apart | 3 |

  @pg16 @serial
  Scenario: slow query LOG records are grouped into top-N rows
    Given a fresh database on PostgreSQL 16
    And a PostgreSQL stderr log fixture:
      """
      2026-07-05 12:04:00 UTC [1]: LOG:  listening on IPv4 address "127.0.0.1", port 5432
      2026-07-05 12:04:01 UTC [1]: LOG:  duration: 1500.250 ms  statement: SELECT * FROM slow_table WHERE id = 42
      2026-07-05 12:04:02 UTC [1]: LOG:  duration: 500.000 ms  statement: SELECT * FROM slow_table WHERE id = 99
      2026-07-05 12:04:03 UTC [1]: LOG:  duration: 10.000 ms
      """
    When the collector snapshots the segment
    Then section 1_022_001 is absent from the segment
    And section 1_026_001 has a row with pattern = SELECT * FROM slow_table WHERE id = ...:
      | count             | 2          |
      | max_duration_ms   | 1500.25    |
      | total_duration_ms | 2000.25    |
      | sample            | SELECT * FROM slow_table WHERE id = 42 |

  @pg16 @serial
  Scenario: lifecycle LOG records carry crash detail and shutdown state
    Given a fresh database on PostgreSQL 16
    And a PostgreSQL stderr log fixture:
      """
      2026-07-05 12:05:00 UTC [1]: LOG:  server process (PID 4242) was terminated by signal 9: Killed
      2026-07-05 12:05:00 UTC [1]: DETAIL:  Failed process was running: SELECT pg_sleep(10)
        FROM lifecycle_probe
      2026-07-05 12:05:01 UTC [1]: LOG:  received fast shutdown request
      2026-07-05 12:05:02 UTC [1]: LOG:  database system is ready to accept connections
      """
    When the collector snapshots the segment
    Then section 1_022_001 has a row with pattern = "server process (...) was terminated by signal ...: Killed":
      | severity | 4 |
      | category | 4 |
      | count    | 1 |
    And section 1_028_001 has a row with kind = 0:
      | pid          | 4242                                         |
      | signal       | 9                                            |
      | query_detail | SELECT pg_sleep(10) FROM lifecycle_probe    |
    And section 1_028_001 has a row with kind = 1:
      | shutdown_mode | fast |
    And section 1_028_001 has a row with kind = 2:
      | message | database system is ready to accept connections |
