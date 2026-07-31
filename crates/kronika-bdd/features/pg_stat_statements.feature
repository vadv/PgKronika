@pg_stat_statements
Feature: Collector seals bounded numeric pg_stat_statements snapshots
  pg_stat_statements is instance-wide and shared by every scenario on the
  boot-once matrix, so each collection scenario resets the view first and avoids
  fixed total row counts: rows from other databases and setup activity between
  reset and snapshot are expected. Each assertion selects its row by queryid,
  obtained from an independent oracle query
  (pg_stat_statements WHERE query LIKE ...), then compares calls and rows as a
  by-key subset check.

  The extension normalizes constants to $n but keeps identifiers, aliases
  and comments, so the seed statements carry distinctive aliases and the
  LIKE patterns anchor on those, never on constant values.

  The numeric source uses pg_stat_statements(false). Query text remains
  required-but-nullable in every versioned row and in the raw API, but is NULL
  even when the source statement is long enough to need a Blob if materialized.
  The last seed statement keeps the previous long-text boundary probe and now
  pins that privacy contract.

  The collector caps its candidate set per axis (top-N by total_exec_time
  and by calls, KRONIKA_PG_MAX_STATEMENTS, default 500). In the version-matrix
  outline the view holds far fewer rows than the cap, so every seeded statement
  must be sealed. A separate N=1 scenario pins overflow behavior.

  @serial @requires_extension
  Scenario Outline: PostgreSQL <major> seals exact numeric statement rows without query text
    Given a fresh database on PostgreSQL <major>
    And a database seeded with:
      """
      CREATE EXTENSION IF NOT EXISTS pg_stat_statements;
      SELECT pg_stat_statements_reset();
      """
    And a database seeded with:
      """
      CREATE TABLE kronika_pgss_t(id int primary key, v int);
      INSERT INTO kronika_pgss_t SELECT g, 0 FROM generate_series(1, 7) AS g;
      """
    And a database seeded with:
      """
      SELECT 41 + 1 AS kronika_calls_probe;
      SELECT 41 + 1 AS kronika_calls_probe;
      SELECT 41 + 1 AS kronika_calls_probe;
      """
    And a database seeded with:
      """
      UPDATE kronika_pgss_t SET v = v + 1 WHERE id <= 5;
      """
    And a database seeded with:
      """
      SELECT /* kronika_blob_pad
      kkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkk
      kkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkk
      kkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkk
      kkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkk
      kkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkk
      kkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkk
      kkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkk
      kkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkk
      kkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkk
      kkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkk
      kkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkk
      kkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkk
      kkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkk
      kkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkk
      kkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkk
      kkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkk
      kkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkk
      kkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkk
      kkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkk
      kkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkk
      kkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkk
      kkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkk
      kkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkk
      kkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkk
      kkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkk
      kkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkk
      kkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkk
      kkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkk
      kkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkk
      kkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkk
      kkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkk
      kkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkk
      kkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkk
      kkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkk
      kkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkk
      kkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkk
      kkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkk
      kkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkk
      kkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkk
      kkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkk
      kkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkk
      kkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkk
      kkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkk
      kkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkk
      kkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkk
      kkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkk
      kkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkk
      kkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkk
      kkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkk
      kkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkk
      kkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkk
      kkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkk
      kkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkk
      kkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkk
      kkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkk
      kkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkk
      kkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkk
      kkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkk
      kkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkk
      kkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkk
      */ 1 AS kronika_blob_probe;
      """
    When the collector snapshots the segment
    Then section <section> has a row for pg_stat_statements query like '%kronika_calls_probe%' with calls = 3 and rows = 3
    And section <section> has a row for pg_stat_statements query like '%UPDATE kronika_pgss_t%' with calls = 1 and rows = 5
    And section <section> has a row for pg_stat_statements query like '%kronika_blob_probe%' with calls = 1 and rows = 1
    And section <section> exposes a null query for pg_stat_statements query like '%kronika_blob_probe%' in PGM and the raw API
    And section <section> has complete numeric statement provenance

    @pg15
    Examples: PostgreSQL 15
      | major | section                     |
      | 15    | pg_stat_statements.pg15_16 |

    @pg16
    Examples: PostgreSQL 16
      | major | section                     |
      | 16    | pg_stat_statements.pg15_16 |

    @pg17
    Examples: PostgreSQL 17
      | major | section                 |
      | 17    | pg_stat_statements.pg17 |

    @pg18
    Examples: PostgreSQL 18
      | major | section                 |
      | 18    | pg_stat_statements.pg18 |

  @pg17 @serial @requires_extension
  Scenario: top-level and nested observations keep separate identities
    Given a fresh database on PostgreSQL 17
    And a database seeded with:
      """
      CREATE EXTENSION IF NOT EXISTS pg_stat_statements;
      SELECT pg_stat_statements_reset();
      SET pg_stat_statements.track = 'all';
      CREATE TABLE kronika_toplevel_identity_probe(id integer);
      DELETE FROM kronika_toplevel_identity_probe;
      DO $$
      BEGIN
        DELETE FROM kronika_toplevel_identity_probe;
      END
      $$;
      """
    When the collector snapshots the segment
    Then section pg_stat_statements.pg17 keeps both toplevel identities for pg_stat_statements query like '%DELETE FROM kronika_toplevel_identity_probe%' with calls = 1 and rows = 0
    And section pg_stat_statements.pg17 has complete numeric statement provenance

  @pg17 @serial @requires_extension
  Scenario: masked query identifiers remain bounded by both configured axes
    Given a fresh database on PostgreSQL 17
    And a database seeded with:
      """
      CREATE EXTENSION IF NOT EXISTS pg_stat_statements;
      CREATE ROLE kronika_pgss_restricted LOGIN;
      GRANT pg_read_all_settings, pg_read_server_files TO kronika_pgss_restricted;
      GRANT EXECUTE ON FUNCTION pg_control_checkpoint() TO kronika_pgss_restricted;
      CREATE SCHEMA kronika_pgss_restricted_scope;
      CREATE VIEW kronika_pgss_restricted_scope.pg_stat_activity AS
        SELECT *
        FROM pg_catalog.pg_stat_activity
        WHERE pid = pg_backend_pid();
      GRANT USAGE ON SCHEMA kronika_pgss_restricted_scope TO kronika_pgss_restricted;
      GRANT SELECT ON kronika_pgss_restricted_scope.pg_stat_activity TO kronika_pgss_restricted;
      ALTER ROLE kronika_pgss_restricted
        SET search_path = kronika_pgss_restricted_scope, public, pg_catalog;
      SELECT pg_stat_statements_reset();
      SELECT pg_sleep(1) AS kronika_masked_time_probe;
      SELECT 7 AS kronika_masked_calls_probe;
      SELECT 7 AS kronika_masked_calls_probe;
      SELECT 7 AS kronika_masked_calls_probe;
      SELECT 7 AS kronika_masked_calls_probe;
      SELECT 7 AS kronika_masked_calls_probe;
      SELECT 7 AS kronika_masked_calls_probe;
      SELECT 7 AS kronika_masked_calls_probe;
      SELECT 7 AS kronika_masked_calls_probe;
      SELECT 7 AS kronika_masked_calls_probe;
      SELECT 7 AS kronika_masked_calls_probe;
      SELECT 7 AS kronika_masked_calls_probe;
      SELECT 7 AS kronika_masked_calls_probe;
      SELECT 1 AS kronika_masked_filler_01;
      SELECT 1 AS kronika_masked_filler_02;
      SELECT 1 AS kronika_masked_filler_03;
      SELECT 1 AS kronika_masked_filler_04;
      SELECT 1 AS kronika_masked_filler_05;
      SELECT 1 AS kronika_masked_filler_06;
      SELECT 1 AS kronika_masked_filler_07;
      SELECT 1 AS kronika_masked_filler_08;
      SELECT 1 AS kronika_masked_filler_09;
      SELECT 1 AS kronika_masked_filler_10;
      SELECT 1 AS kronika_masked_filler_11;
      SELECT 1 AS kronika_masked_filler_12;
      """
    And the collector connects to the scenario database as role "kronika_pgss_restricted"
    And the collector runs with env "KRONIKA_PG_MAX_STATEMENTS" = "1"
    When the collector snapshots the segment
    Then section pg_stat_statements.pg17 has exactly two bounded rows with masked query identifiers

  @pg17 @serial @requires_extension
  Scenario: a successful rediscovered source supersedes a stale cached-source failure
    Given a fresh database on PostgreSQL 17
    And a database seeded with:
      """
      CREATE EXTENSION IF NOT EXISTS pg_stat_statements;
      SELECT pg_stat_statements_reset();
      SELECT 1 AS kronika_stale_source_first;
      """
    And a second database seeded with:
      """
      SELECT 1;
      """
    When one collector replaces a stale pg_stat_statements source with the second database
    Then section pg_stat_statements.pg17 records complete success after the cached-source failure
