Feature: Collector reads pg_stat_io
  pg_stat_io (type 1_009_001 on PG16-17, type 1_009_002 on PG18) does not
  exist before PG16. After a shared stats reset, a CREATE TABLE, INSERT, and
  CHECKPOINT force visible client-backend I/O. Each scenario checks the
  version-specific layout, verifies label resolution through the dictionary,
  checks op_bytes against an independent oracle on V1, and confirms stats_reset
  does not exceed the snapshot timestamp via a ceiling oracle.

  @pg16 @serial
  Scenario: PG16 seals the V1 layout for pg_stat_io with op_bytes
    Given a fresh database on PostgreSQL 16
    And a database seeded with:
      """
      SELECT pg_stat_reset_shared('io');
      CREATE TABLE t(id int);
      INSERT INTO t VALUES (1);
      CHECKPOINT;
      """
    When the collector snapshots the segment
    Then section 1_009_001 has a pg_stat_io row for (client backend, relation, normal):
      | op_bytes | 8192 |
    And section 1_009_001 op_bytes matches the subset oracle:
      """
      SELECT op_bytes
      FROM pg_stat_io
      WHERE backend_type = 'client backend'
        AND object = 'relation'
        AND context = 'normal'
      """
    And section 1_009_001 backend_type matches the subset oracle:
      """
      SELECT DISTINCT backend_type FROM pg_stat_io
      """
    And section 1_009_001 stats_reset matches the ceiling oracle:
      """
      SELECT (EXTRACT(EPOCH FROM NOW()) * 1000000)::bigint
      """
    And section 1_009_002 is absent from the segment

  @pg17 @serial
  Scenario: PG17 seals the V1 layout for pg_stat_io with op_bytes
    Given a fresh database on PostgreSQL 17
    And a database seeded with:
      """
      SELECT pg_stat_reset_shared('io');
      CREATE TABLE t(id int);
      INSERT INTO t VALUES (1);
      CHECKPOINT;
      """
    When the collector snapshots the segment
    Then section 1_009_001 has a pg_stat_io row for (client backend, relation, normal):
      | op_bytes | 8192 |
    And section 1_009_001 op_bytes matches the subset oracle:
      """
      SELECT op_bytes
      FROM pg_stat_io
      WHERE backend_type = 'client backend'
        AND object = 'relation'
        AND context = 'normal'
      """
    And section 1_009_001 backend_type matches the subset oracle:
      """
      SELECT DISTINCT backend_type FROM pg_stat_io
      """
    And section 1_009_001 stats_reset matches the ceiling oracle:
      """
      SELECT (EXTRACT(EPOCH FROM NOW()) * 1000000)::bigint
      """
    And section 1_009_002 is absent from the segment

  @pg18 @serial
  Scenario: PG18 seals the V2 layout for pg_stat_io with per-op byte counters
    Given a fresh database on PostgreSQL 18
    And a database seeded with:
      """
      SELECT pg_stat_reset_shared('io');
      CREATE TABLE t(id int);
      INSERT INTO t VALUES (1);
      CHECKPOINT;
      """
    When the collector snapshots the segment
    Then section 1_009_002 has a pg_stat_io row for (client backend, relation, normal):
      | write_bytes | 0 |
    And section 1_009_002 backend_type matches the subset oracle:
      """
      SELECT DISTINCT backend_type FROM pg_stat_io
      """
    And section 1_009_002 stats_reset matches the ceiling oracle:
      """
      SELECT (EXTRACT(EPOCH FROM NOW()) * 1000000)::bigint
      """
    And section 1_009_001 is absent from the segment
