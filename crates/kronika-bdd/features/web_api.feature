@web
Feature: The web API serves collected sections over HTTP
  The collector output that the direct-decode scenarios check is also served
  through the in-process JSON router. A matching row proves the reader query
  layer and the HTTP serialization agree with the sealed segment, end to end.

  @pg17 @serial
  Scenario: the web API returns locale-neutral Problem Details
    Given a fresh database on PostgreSQL 17
    When the collector snapshots the segment
    Then an invalid web API request returns locale-neutral Problem Details

  @pg17 @serial
  Scenario: the web API serves the reset archiver row
    Given a fresh database on PostgreSQL 17
    And a database seeded with:
      """
      SELECT pg_stat_reset_shared('archiver');
      """
    When the collector snapshots the segment
    Then the web API serves section pg_stat_archiver with one row:
      | archived_count    | 0    |
      | failed_count      | 0    |
      | last_archived_wal | null |
      | last_failed_wal   | null |

  @pg17 @serial
  Scenario: the web API serves the standalone primary replication row
    Given a fresh database on PostgreSQL 17
    When the collector snapshots the segment
    Then the web API serves section replication_instance with one row:
      | is_in_recovery      | false |
      | streaming_replicas  | 0     |
      | replay_lag_s        | null  |
      | wal_receiver_status | null  |
      | slot_name           | null  |

  @pg17 @serial
  Scenario: the web API serves the isolated database row selected by name
    Given a fresh database on PostgreSQL 17
    And a database seeded with:
      """
      SELECT pg_stat_reset();
      CREATE TABLE probe(v int);
      INSERT INTO probe SELECT generate_series(1, 10);
      """
    When the collector snapshots the segment
    Then the web API serves section pg_stat_database.pg14_17 with a row where datname = [scenario database]:
      | datallowconn  | true  |
      | datistemplate | false |
      | datconnlimit  | -1    |

  @pg17 @serial
  Scenario: the web API serves a prepared-transaction row selected by database
    Given a fresh database on PostgreSQL 17
    And a database seeded with:
      """
      CREATE TABLE probe (id int);
      """
    And the pg_prepared_xacts transaction is prepared:
      """
      BEGIN;
      INSERT INTO probe VALUES (1);
      PREPARE TRANSACTION 'kronika_bdd_prepared_xacts_probe';
      """
    When the collector snapshots the segment
    Then the web API serves section pg_prepared_xacts with a row where datname = [scenario database]:
      | prepared_count | 1 |

  @pg16 @serial
  Scenario: the web API serves a pg_stat_io row selected by its labels
    Given a fresh database on PostgreSQL 16
    And a database seeded with:
      """
      SELECT pg_stat_reset_shared('io');
      CREATE TABLE t(id int);
      INSERT INTO t VALUES (1);
      CHECKPOINT;
      """
    When the collector snapshots the segment
    Then the web API serves section pg_stat_io.pg16_17 with a row where backend_type = "client backend" and object = "relation" and context = "normal":
      | op_bytes | 8192 |
