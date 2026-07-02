Feature: Collector reads pg_stat_user_tables and pg_stat_user_indexes across databases
  Per-table and per-index statistics are collected once per selected table/index
  per database. datname is dictionary-backed: a row from database A carries A's
  datname even when both databases hold an identically named table. Two isolated
  databases are created per scenario so no scenario shares table state with another.

  @pg17 @serial
  Scenario: probe table row appears in pg_stat_user_tables with correct insert count for each database
    Given a fresh database on PostgreSQL 17
    And a database seeded with:
      """
      CREATE TABLE kronika_ut_probe (id int primary key, payload text);
      INSERT INTO kronika_ut_probe SELECT g, repeat('x', 16) FROM generate_series(1, 200) g;
      ANALYZE kronika_ut_probe;
      """
    And a second database seeded with:
      """
      CREATE TABLE kronika_ut_probe (id int primary key, payload text);
      INSERT INTO kronika_ut_probe SELECT g, repeat('x', 16) FROM generate_series(1, 200) g;
      ANALYZE kronika_ut_probe;
      """
    When the collector snapshots the segment
    Then section 1_013_003 has a pg_stat_user_tables row for table "kronika_ut_probe" in the primary database:
      | n_tup_ins | >= 200 |
    Then section 1_013_003 has a pg_stat_user_tables row for table "kronika_ut_probe" in the second database:
      | n_tup_ins | >= 200 |
    Then pg_stat_user_tables n_tup_ins for "kronika_ut_probe" in the primary database matches the subset oracle:
      """
      SELECT n_tup_ins FROM pg_stat_user_tables WHERE relname = 'kronika_ut_probe'
      """
    Then pg_stat_user_tables n_tup_ins for "kronika_ut_probe" in the second database matches the subset oracle:
      """
      SELECT n_tup_ins FROM pg_stat_user_tables WHERE relname = 'kronika_ut_probe'
      """

  @pg17 @serial
  Scenario: probe index row appears in pg_stat_user_indexes with nonzero scan count for each database
    Given a fresh database on PostgreSQL 17
    And a database seeded with:
      """
      CREATE TABLE kronika_ut_probe (id int primary key, payload text);
      INSERT INTO kronika_ut_probe SELECT g, md5(g::text) FROM generate_series(1, 200) g;
      CREATE INDEX kronika_ut_probe_idx ON kronika_ut_probe (payload);
      ANALYZE kronika_ut_probe;
      SET enable_seqscan = off;
      SELECT payload FROM kronika_ut_probe WHERE payload = md5(42::text);
      SELECT pg_stat_force_next_flush();
      """
    And a second database seeded with:
      """
      CREATE TABLE kronika_ut_probe (id int primary key, payload text);
      INSERT INTO kronika_ut_probe SELECT g, md5(g::text) FROM generate_series(1, 200) g;
      CREATE INDEX kronika_ut_probe_idx ON kronika_ut_probe (payload);
      ANALYZE kronika_ut_probe;
      SET enable_seqscan = off;
      SELECT payload FROM kronika_ut_probe WHERE payload = md5(42::text);
      SELECT pg_stat_force_next_flush();
      """
    When the collector snapshots the segment
    Then section 1_014_002 has a pg_stat_user_indexes row for index "kronika_ut_probe_idx" in the primary database:
      | idx_scan     | >= 1  |
      | indisprimary | false |
    Then section 1_014_002 has a pg_stat_user_indexes row for index "kronika_ut_probe_idx" in the second database:
      | idx_scan     | >= 1  |
      | indisprimary | false |
    Then pg_stat_user_indexes idx_scan for "kronika_ut_probe_idx" in the primary database matches the subset oracle:
      """
      SELECT idx_scan FROM pg_stat_user_indexes WHERE indexrelname = 'kronika_ut_probe_idx'
      """
    Then pg_stat_user_indexes idx_scan for "kronika_ut_probe_idx" in the second database matches the subset oracle:
      """
      SELECT idx_scan FROM pg_stat_user_indexes WHERE indexrelname = 'kronika_ut_probe_idx'
      """
