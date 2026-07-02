Feature: Collector writes pg_store_plans (vadv fork) to section 1_004_001
  The PostgreSQL 17 and 18 test images include the vadv pg_store_plans fork and
  run with compute_query_id=on. The collector reads top plans without texts
  through pg_store_plans(false), then fetches plan text through
  pg_store_plans_get_plan. The oracle joins the live view to
  pg_stat_statements through queryid_stat_statements; with compute_query_id=on,
  two statements with the same plan shape remain separate rows.

  @pg17 @serial
  Scenario: two statements with one plan shape stay separate rows with fetched texts
    Given a fresh database on PostgreSQL 17
    And a database seeded with:
      """
      CREATE EXTENSION pg_store_plans;
      CREATE EXTENSION pg_stat_statements;
      CREATE TABLE kronika_psp_probe(id int PRIMARY KEY, payload text);
      INSERT INTO kronika_psp_probe SELECT g, repeat('x', 32) FROM generate_series(1, 100) g;
      SELECT pg_store_plans_reset();
      SELECT pg_stat_statements_reset();
      """
    And a database seeded with:
      """
      SELECT count(*)  AS kronika_psp_marker_a FROM kronika_psp_probe;
      SELECT count(*)  AS kronika_psp_marker_a FROM kronika_psp_probe;
      SELECT count(*)  AS kronika_psp_marker_a FROM kronika_psp_probe;
      SELECT count(id) AS kronika_psp_marker_b FROM kronika_psp_probe;
      SELECT count(id) AS kronika_psp_marker_b FROM kronika_psp_probe;
      """
    When the collector snapshots the segment
    Then section 1_004_001 has a pg_store_plans row for query like '%kronika_psp_marker_a%' with calls = 3 and a resolvable plan
    And section 1_004_001 has a pg_store_plans row for query like '%kronika_psp_marker_b%' with calls = 2 and a resolvable plan
    And section 1_003_001 is absent from the segment

  @pg18 @serial
  Scenario: PG18 writes the same vadv layout
    Given a fresh database on PostgreSQL 18
    And a database seeded with:
      """
      CREATE EXTENSION pg_store_plans;
      CREATE EXTENSION pg_stat_statements;
      CREATE TABLE kronika_psp_probe(id int PRIMARY KEY, payload text);
      INSERT INTO kronika_psp_probe SELECT g, repeat('x', 32) FROM generate_series(1, 50) g;
      SELECT pg_store_plans_reset();
      SELECT pg_stat_statements_reset();
      """
    And a database seeded with:
      """
      SELECT count(*) AS kronika_psp_marker FROM kronika_psp_probe;
      """
    When the collector snapshots the segment
    Then section 1_004_001 has a pg_store_plans row for query like '%kronika_psp_marker%' with calls = 1 and a resolvable plan
    And section 1_003_001 is absent from the segment
