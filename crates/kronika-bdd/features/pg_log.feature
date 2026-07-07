@pg_log
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
    Then section pg_log_errors has a row with pattern = relation "..." does not exist:
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
    Then section pg_log_errors has a row with pattern = "server process (...) was terminated by signal ...: Killed":
      | severity | 4 |
      | category | 4 |
      | count    | 1 |
    And section pg_log_errors has a row with pattern = "server process (...) was terminated by signal ...: Segmentation fault":
      | severity | 4 |
      | category | 6 |
      | count    | 1 |
    And section pg_log_errors has a row with pattern = "terminating connection because of crash of another server process":
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
    Then section pg_log_errors has a row with pattern = "deadlock detected":
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
    Then section pg_log_errors is absent from the segment
    And section pg_log_checkpoints has a row with phase = 0:
      | reason | time |
    And section pg_log_checkpoints has a row with phase = 1:
      | buffers_written | 128    |
      | write_ms        | 1234.0 |
      | sync_ms         | 56.0   |
      | total_ms        | 1500.0 |
      | wal_added       | 0      |
      | wal_removed     | 1      |
      | wal_recycled    | 2      |
      | sync_files      | 7      |
      | distance_kb     | 4096   |
      | estimate_kb     | 8192   |
      | longest_sync_ms | 40.0   |
      | average_sync_ms | 8.0    |
    And section pg_log_checkpoints has a row with phase = 2:
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
    Then section pg_log_errors is absent from the segment
    And section pg_log_slow_queries has a row with pattern = SELECT * FROM slow_table WHERE id = ...:
      | count             | 2          |
      | max_duration_ms   | 1500.25    |
      | total_duration_ms | 2000.25    |
      | sample            | SELECT * FROM slow_table WHERE id = 42 |

  @pg16 @serial
  Scenario: autovacuum LOG records are typed without becoming errors
    Given a fresh database on PostgreSQL 16
    And a PostgreSQL stderr log fixture:
      """
      2026-07-05 12:06:00 UTC [1]: LOG:  automatic vacuum of table "mydb.public.orders": index scans: 1
        pages: 10 removed, 20 remain, 0 skipped due to pins, 0 skipped frozen
        tuples: 30 removed, 40 remain, 5 are dead but not yet removable, oldest xmin: 123
        buffer usage: 100 hits, 2 misses, 3 dirtied
        avg read rate: 1.500 MB/s, avg write rate: 2.500 MB/s
        WAL usage: 15 records, 2 full page images, 4096 bytes
        system usage: CPU: user: 0.12 s, system: 0.34 s, elapsed: 5.67 s
      2026-07-05 12:06:00 UTC [1]: LOG:  automatic vacuum launcher started
      2026-07-05 12:06:01 UTC [1]: LOG:  automatic analyze of table "tpl-service.bucket_90.posting_sender"
        buffer usage: 1843 hits, 3 reads, 4 dirtied
        avg read rate: 0.500 MB/s, avg write rate: 0.000 MB/s
        system usage: CPU: user: 0.02 s, system: 0.01 s, elapsed: 3.60 s
      2026-07-05 12:06:02 UTC [1]: LOG:  listening on Unix socket "/tmp/.s.PGSQL.5432"
      """
    When the collector snapshots the segment
    Then section pg_log_errors is absent from the segment
    And section pg_log_autovacuum has 2 rows
    And section pg_log_autovacuum has a row with relation = mydb.public.orders:
      | kind                       | 0      |
      | index_scans                | 1      |
      | pages_removed              | 10     |
      | pages_remaining            | 20     |
      | tuples_removed             | 30     |
      | tuples_remaining           | 40     |
      | tuples_dead_not_removable  | 5      |
      | elapsed_ms                 | 5670.0 |
      | buffer_hits                | 100    |
      | buffer_misses              | 2      |
      | buffer_dirtied             | 3      |
      | avg_read_rate_mbs          | 1.5    |
      | avg_write_rate_mbs         | 2.5    |
      | cpu_user_ms                | 120.0  |
      | cpu_system_ms              | 340.0  |
      | wal_records                | 15     |
      | wal_fpi                    | 2      |
      | wal_bytes                  | 4096   |
      | dict_dropped_fields        | 0      |
    And section pg_log_autovacuum has a row with relation = tpl-service.bucket_90.posting_sender:
      | kind                      | 1      |
      | pages_removed             | null   |
      | tuples_removed            | null   |
      | buffer_hits               | 1843   |
      | buffer_misses             | 3      |
      | buffer_dirtied            | 4      |
      | avg_read_rate_mbs         | 0.5    |
      | avg_write_rate_mbs        | 0.0    |
      | cpu_user_ms               | 20.0   |
      | cpu_system_ms             | 10.0   |
      | elapsed_ms                | 3600.0 |
      | wal_records               | null   |
      | dict_dropped_fields       | 0      |

  @pg16 @serial
  Scenario: lock-wait LOG records are typed without becoming errors
    Given a fresh database on PostgreSQL 16
    And a PostgreSQL stderr log fixture:
      """
      2026-07-05 12:07:00 UTC [70]: LOG:  process 70 still waiting for ShareLock on transaction 12345678 after 30009.004 ms
      2026-07-05 12:07:00 UTC [70]: DETAIL:  Process holding the lock: 80. Wait queue: 70.
        Wait queue continues on the next line.
      2026-07-05 12:07:00 UTC [70]: CONTEXT:  while updating tuple (0,1) in relation "accounts"
        during lock wait probe
      2026-07-05 12:07:00 UTC [70]: STATEMENT:  UPDATE accounts SET balance = balance + 1 WHERE id = 1
        RETURNING balance
      2026-07-05 12:07:00 UTC [70]: LOG:  process 70 still waiting for ShareLock
      2026-07-05 12:07:01 UTC [70]: LOG:  process 70 acquired ShareLock on transaction 12345678 after 30010.004 ms
      """
    When the collector snapshots the segment
    Then section pg_log_errors is absent from the segment
    And section pg_log_lock_waits has 2 rows
    And section pg_log_lock_waits has a row with kind = 0:
      | pid          | 70                                                                                                      |
      | lock_mode    | ShareLock                                                                                               |
      | lock_target  | transaction 12345678                                                                                    |
      | duration_ms  | 30009.004                                                                                               |
      | detail       | Process holding the lock: 80. Wait queue: 70. Wait queue continues on the next line.                    |
      | context      | while updating tuple (0,1) in relation "accounts" during lock wait probe                                |
      | statement    | UPDATE accounts SET balance = balance + 1 WHERE id = 1 RETURNING balance                                |
      | dict_dropped_fields | 0                                                                                                 |
    And section pg_log_lock_waits has a row with kind = 1:
      | pid                 | 70                   |
      | lock_mode           | ShareLock            |
      | lock_target         | transaction 12345678 |
      | duration_ms         | 30010.004            |
      | detail              | null                 |
      | context             | null                 |
      | statement           | null                 |
      | dict_dropped_fields | 0                    |

  @pg16 @serial
  Scenario: temporary-file LOG records are typed without becoming errors
    Given a fresh database on PostgreSQL 16
    And a PostgreSQL stderr log fixture:
      """
      2026-07-05 12:08:00 UTC [1]: LOG:  temporary file: path "base/pgsql_tmp/pgsql_tmp15967.0", size 200204288
      2026-07-05 12:08:00 UTC [1]: STATEMENT:  SELECT * FROM big_sort ORDER BY payload
        LIMIT 100
      2026-07-05 12:08:01 UTC [1]: LOG:  temporary file cleanup complete
      2026-07-05 12:08:02 UTC [1]: LOG:  temporary file: path "base/pgsql_tmp/pgsql_tmp15967.no_size"
      2026-07-05 12:08:03 UTC [1]: LOG:  temporary file: path "base/pgsql_tmp/pgsql_tmp15967.1", size 0
      """
    When the collector snapshots the segment
    Then section pg_log_errors is absent from the segment
    And section pg_log_temp_files has 2 rows
    And section pg_log_temp_files has a row with path = base/pgsql_tmp/pgsql_tmp15967.0:
      | size_bytes          | 200204288                                         |
      | statement           | SELECT * FROM big_sort ORDER BY payload LIMIT 100 |
      | dict_dropped_fields | 0                                                 |
    And section pg_log_temp_files has a row with path = base/pgsql_tmp/pgsql_tmp15967.1:
      | size_bytes          | 0    |
      | statement           | null |
      | dict_dropped_fields | 0    |

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
    Then section pg_log_errors has a row with pattern = "server process (...) was terminated by signal ...: Killed":
      | severity | 4 |
      | category | 4 |
      | count    | 1 |
    And section pg_log_lifecycle has a row with kind = 0:
      | pid          | 4242                                         |
      | signal       | 9                                            |
      | query_detail | SELECT pg_sleep(10) FROM lifecycle_probe    |
    And section pg_log_lifecycle has a row with kind = 1:
      | shutdown_mode | fast |
    And section pg_log_lifecycle has a row with kind = 2:
      | message | database system is ready to accept connections |

  @pg16 @serial
  Scenario: Russian stderr LOG records use PostgreSQL NLS strings
    Given a fresh database on PostgreSQL 16
    And a PostgreSQL stderr log fixture:
      """
      2026-07-05 12:09:00 UTC [1]: СООБЩЕНИЕ:  начата контрольная точка: time
      2026-07-05 12:09:01 UTC [1]: СООБЩЕНИЕ:  контрольная точка завершена: записано буферов: 128 (0.2%); добавлено файлов WAL 0, удалено: 1, переработано: 2; запись=1.234 сек., синхр.=0.056 сек., всего=1.500 сек.; синхронизировано_файлов=7, самая_долгая_синхр.=0.040 сек., средняя=0.008 сек.; расстояние=4096 КБ, ожидалось=8192 КБ
      2026-07-05 12:09:02 UTC [1]: СООБЩЕНИЕ:  продолжительность: 1500.250 мс, оператор: SELECT * FROM slow_table WHERE id = 42
      2026-07-05 12:09:03 UTC [1]: СООБЩЕНИЕ:  автоматическая очистка таблицы "mydb.public.orders": сканирований индекса: 1
        страниц удалено: 10, осталось: 20, просканировано: 30 (50.00% от общего числа)
        версий строк: удалено: 30, осталось: 40, «мёртвых», но ещё не подлежащих удалению: 5
        использование буфера: попаданий: 100, промахов: 2, «грязных» записей: 3
        средняя скорость чтения: 1.500 МБ/с, средняя скорость записи: 2.500 МБ/с
        использование WAL: записей: 15, полных образов страниц: 2, байт: 4096
        CPU: пользов.: 0.12 с, система: 0.34 с, прошло: 5.67 с
      2026-07-05 12:09:04 UTC [70]: СООБЩЕНИЕ:  процесс 70 продолжает ожидать в режиме ShareLock блокировку "transaction 12345678" в течение 30009.004 мс
      2026-07-05 12:09:04 UTC [70]: ОПЕРАТОР:  UPDATE accounts SET balance = balance + 1 WHERE id = 1
      2026-07-05 12:09:05 UTC [1]: СООБЩЕНИЕ:  временный файл: путь "base/pgsql_tmp/pgsql_tmp15967.2", размер 200204288
      2026-07-05 12:09:05 UTC [1]: ОПЕРАТОР:  SELECT * FROM big_sort ORDER BY payload
      2026-07-05 12:09:06 UTC [1]: СООБЩЕНИЕ:  процесс сервера (PID 4242) был завершён по сигналу 9: Killed
      2026-07-05 12:09:06 UTC [1]: ПОДРОБНОСТИ:  Завершившийся процесс выполнял действие: SELECT pg_sleep(10)
      2026-07-05 12:09:07 UTC [1]: СООБЩЕНИЕ:  получен запрос на "вежливое" выключение
      2026-07-05 12:09:08 UTC [1]: СООБЩЕНИЕ:  система БД готова принимать подключения
      """
    When the collector snapshots the segment
    Then section pg_log_checkpoints has 2 rows
    And section pg_log_checkpoints has a row with phase = 0:
      | reason | time |
    And section pg_log_checkpoints has a row with phase = 1:
      | buffers_written | 128    |
      | write_ms        | 1234.0 |
      | sync_ms         | 56.0   |
      | total_ms        | 1500.0 |
      | wal_added       | 0      |
      | wal_removed     | 1      |
      | wal_recycled    | 2      |
      | sync_files      | 7      |
      | distance_kb     | 4096   |
      | estimate_kb     | 8192   |
      | longest_sync_ms | 40.0   |
      | average_sync_ms | 8.0    |
    And section pg_log_slow_queries has a row with pattern = SELECT * FROM slow_table WHERE id = ...:
      | count             | 1                                      |
      | max_duration_ms   | 1500.25                                |
      | total_duration_ms | 1500.25                                |
      | sample            | SELECT * FROM slow_table WHERE id = 42 |
    And section pg_log_autovacuum has a row with relation = mydb.public.orders:
      | kind                      | 0      |
      | index_scans               | 1      |
      | pages_removed             | 10     |
      | pages_remaining           | 20     |
      | tuples_removed            | 30     |
      | tuples_remaining          | 40     |
      | tuples_dead_not_removable | 5      |
      | elapsed_ms                | 5670.0 |
      | buffer_hits               | 100    |
      | buffer_misses             | 2      |
      | buffer_dirtied            | 3      |
      | avg_read_rate_mbs         | 1.5    |
      | avg_write_rate_mbs        | 2.5    |
      | cpu_user_ms               | 120.0  |
      | cpu_system_ms             | 340.0  |
      | wal_records               | 15     |
      | wal_fpi                   | 2      |
      | wal_bytes                 | 4096   |
    And section pg_log_lock_waits has a row with kind = 0:
      | pid         | 70                   |
      | lock_mode   | ShareLock            |
      | lock_target | transaction 12345678 |
      | duration_ms | 30009.004            |
      | statement   | UPDATE accounts SET balance = balance + 1 WHERE id = 1 |
    And section pg_log_temp_files has a row with path = base/pgsql_tmp/pgsql_tmp15967.2:
      | size_bytes | 200204288                                  |
      | statement  | SELECT * FROM big_sort ORDER BY payload    |
    And section pg_log_errors has a row with pattern = "процесс сервера (...) был завершён по сигналу ...: Killed":
      | severity | 4 |
      | category | 4 |
      | count    | 1 |
    And section pg_log_lifecycle has a row with kind = 0:
      | pid          | 4242                                      |
      | signal       | 9                                         |
      | query_detail | SELECT pg_sleep(10)                      |
    And section pg_log_lifecycle has a row with kind = 1:
      | shutdown_mode | smart |
    And section pg_log_lifecycle has a row with kind = 2:
      | message | система БД готова принимать подключения |
