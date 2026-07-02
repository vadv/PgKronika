Feature: Collector seals pg_stat_database rows with catalog fields and counter data
  pg_stat_database has one row per database plus a datid=0 shared-objects row.
  The row for the scenario's isolated database carries correct pg_database catalog
  fields and records at least the rows inserted during setup.

  @pg17 @serial
  Scenario: isolated database row carries catalog fields and captures inserted rows
    Given a fresh database on PostgreSQL 17
    And a database seeded with:
      """
      SELECT pg_stat_reset();
      CREATE TABLE probe(v int);
      INSERT INTO probe SELECT generate_series(1, 10);
      """
    When the collector snapshots the segment
    Then section 1_005_003 has a row with datname = [scenario database]:
      | datallowconn  | true  |
      | datistemplate | false |
      | datconnlimit  | -1    |
    And section 1_005_003 tup_inserted matches the floor oracle:
      """
      SELECT 10::bigint
      """
    And section 1_005_003 datallowconn matches the subset oracle:
      """
      SELECT datallowconn FROM pg_database WHERE datname = current_database()
      """
    And section 1_005_003 datistemplate matches the subset oracle:
      """
      SELECT datistemplate FROM pg_database WHERE datname = current_database()
      """
