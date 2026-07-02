Feature: Collector opens per-database pool connections
  The pool enumerates non-template databases that grant CONNECT and opens one
  connection per database. pg_stat_user_tables (section 1_013_003 on PG 16-17)
  is collected only through those per-database connections, and each sealed
  row carries the datname of the connection that collected it. A row attributed
  to a database proves the pool opened that database connection; if fan-out
  breaks, that database's rows are absent. PG 17 covers this behavior because
  the pool contract is independent of the server major.

  @pg17 @serial
  Scenario: user tables are sealed from two databases through per-database connections
    Given a fresh database on PostgreSQL 17
    And a database seeded with:
      """
      CREATE TABLE pool_probe_scenario (id int PRIMARY KEY);
      INSERT INTO pool_probe_scenario VALUES (1);
      """
    And a second database seeded with:
      """
      CREATE TABLE pool_probe_extra (id int PRIMARY KEY);
      INSERT INTO pool_probe_extra VALUES (1);
      """
    When the collector snapshots the segment
    Then section 1_013_003 has one row for table "pool_probe_scenario" attributed to the scenario database
    And section 1_013_003 has one row for table "pool_probe_extra" attributed to the second database
    And section 1_013_003 relid matches the subset oracle:
      """
      SELECT c.oid::bigint FROM pg_class c WHERE c.relname = 'pool_probe_scenario'
      """
