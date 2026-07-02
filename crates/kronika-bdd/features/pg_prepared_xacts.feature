Feature: Collector seals pg_prepared_xacts rows per database
  pg_prepared_xacts aggregates two-phase-commit transactions waiting for
  resolution, grouped by database. When no prepared transactions exist the
  section is absent; once a transaction is prepared in a database, one row
  appears for that database. The cluster is booted with
  max_prepared_transactions=16, so 2PC is available on every matrix version.

  @pg17 @serial
  Scenario: a prepared transaction appears as one row for the scenario database
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
    Then section 1_010_001 has a pg_prepared_xacts row for the scenario database:
      | prepared_count | 1 |
    And section 1_010_001 prepared_count matches the exact oracle:
      """
      SELECT COUNT(*)::bigint
      FROM pg_prepared_xacts
      WHERE database = current_database()
      """
