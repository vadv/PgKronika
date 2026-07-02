Feature: Collector reads pg_stat_progress_vacuum
  pg_stat_progress_vacuum has rows only while a VACUUM is running. With no active
  VACUUM the section is absent from the sealed segment. When a manual VACUUM is
  running the collector captures the backend pid, the database OID, the table OID,
  the is_autovacuum flag, and the dictionary-backed datname; these are verified
  against catalog oracles that remain stable after the vacuum finishes.

  @pg17 @serial
  Scenario: no active VACUUM produces no pg_stat_progress_vacuum section
    Given a fresh database on PostgreSQL 17
    When the collector snapshots the segment
    Then section 1_012_001 is absent

  @pg17 @slow @serial
  Scenario: a running manual VACUUM is captured with relid and datid matching the catalog
    Given a fresh database on PostgreSQL 17
    And a database seeded with:
      """
      CREATE TABLE kronika_vac_probe (id int) WITH (autovacuum_enabled = false);
      INSERT INTO kronika_vac_probe SELECT generate_series(1, 50000);
      DELETE FROM kronika_vac_probe WHERE id % 2 = 0;
      """
    And session "V" runs VACUUM in the background:
      """
      SET vacuum_cost_delay = 100;
      SET vacuum_cost_limit = 1;
      VACUUM kronika_vac_probe;
      """
    And pg_stat_progress_vacuum shows session "V"
    When the collector snapshots the segment
    Then section 1_012_001 has a row for session "V":
      | is_autovacuum | false |
    And section 1_012_001 datid matches the exact oracle:
      """
      SELECT oid::bigint FROM pg_database WHERE datname = current_database()
      """
    And section 1_012_001 relid matches the exact oracle:
      """
      SELECT 'kronika_vac_probe'::regclass::oid::bigint
      """
