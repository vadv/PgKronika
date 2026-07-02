Feature: Collector writes upstream pg_store_plans to section 1_003_001
  The PostgreSQL 15 and 16 test images include the ossc upstream fork. Unlike
  the vadv fork, the upstream keys an entry by (userid, dbid, queryid, planid)
  with the real core query id, so plans stay per-statement and queryid joins
  pg_stat_statements directly. The view carries plan text inline; the
  collector reads it in one query with a server-side per-row truncation.

  @pg15 @serial
  Scenario: a repeated statement seals its own plan row
    Given a fresh database on PostgreSQL 15
    And a database seeded with:
      """
      CREATE EXTENSION pg_store_plans;
      CREATE EXTENSION pg_stat_statements;
      CREATE TABLE kronika_ossc_probe(id int PRIMARY KEY, payload text);
      INSERT INTO kronika_ossc_probe SELECT g, repeat('x', 32) FROM generate_series(1, 100) g;
      SELECT pg_store_plans_reset();
      SELECT pg_stat_statements_reset();
      """
    And a database seeded with:
      """
      SELECT count(*) AS kronika_ossc_marker FROM kronika_ossc_probe;
      SELECT count(*) AS kronika_ossc_marker FROM kronika_ossc_probe;
      SELECT count(*) AS kronika_ossc_marker FROM kronika_ossc_probe;
      """
    When the collector snapshots the segment
    Then section 1_003_001 has an ossc pg_store_plans row for query like '%kronika_ossc_marker%' with calls = 3 and a resolvable plan
    And section 1_004_001 is absent from the segment

  @pg16 @serial
  Scenario: statements sharing a plan shape keep separate per-query rows
    Given a fresh database on PostgreSQL 16
    And a database seeded with:
      """
      CREATE EXTENSION pg_store_plans;
      CREATE EXTENSION pg_stat_statements;
      CREATE TABLE kronika_ossc_split(id int PRIMARY KEY);
      INSERT INTO kronika_ossc_split SELECT g FROM generate_series(1, 100) g;
      SELECT pg_store_plans_reset();
      SELECT pg_stat_statements_reset();
      """
    And a database seeded with:
      """
      SELECT id FROM kronika_ossc_split WHERE id = 1;
      SELECT id FROM kronika_ossc_split WHERE id = 2;
      SELECT id FROM kronika_ossc_split WHERE id = 3 AND true;
      """
    When the collector snapshots the segment
    Then section 1_003_001 has an ossc pg_store_plans row for query like '%kronika_ossc_split WHERE id = $1' with calls = 2 and a resolvable plan
    And section 1_003_001 has an ossc pg_store_plans row for query like '%kronika_ossc_split WHERE id = $1 AND%' with calls = 1 and a resolvable plan
    And section 1_004_001 is absent from the segment
