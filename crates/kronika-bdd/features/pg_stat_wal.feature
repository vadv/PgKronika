Feature: Collector reads pg_stat_wal
  Section 1_007_001 is the PG15-17 pg_stat_wal layout. PG18 uses section
  1_007_002 after write/sync counters moved out of pg_stat_wal. Each scenario
  checks the selected layout, proves the other layout is absent, and compares
  stats_reset against PostgreSQL.

  @pg15 @serial
  Scenario: PG15 seals the V1 pg_stat_wal layout
    Given a fresh database on PostgreSQL 15
    When the collector snapshots the segment
    Then section 1_007_001 uses the PG15-17 pg_stat_wal layout
    And section 1_007_002 is absent from the segment
    And section 1_007_001 stats_reset matches the exact oracle:
      """
      SELECT (EXTRACT(EPOCH FROM stats_reset) * 1000000)::bigint
      FROM pg_stat_wal
      """

  @pg16 @serial
  Scenario: PG16 seals the V1 pg_stat_wal layout
    Given a fresh database on PostgreSQL 16
    When the collector snapshots the segment
    Then section 1_007_001 uses the PG15-17 pg_stat_wal layout
    And section 1_007_002 is absent from the segment
    And section 1_007_001 stats_reset matches the exact oracle:
      """
      SELECT (EXTRACT(EPOCH FROM stats_reset) * 1000000)::bigint
      FROM pg_stat_wal
      """

  @pg17 @serial
  Scenario: PG17 seals the V1 pg_stat_wal layout
    Given a fresh database on PostgreSQL 17
    When the collector snapshots the segment
    Then section 1_007_001 uses the PG15-17 pg_stat_wal layout
    And section 1_007_002 is absent from the segment
    And section 1_007_001 stats_reset matches the exact oracle:
      """
      SELECT (EXTRACT(EPOCH FROM stats_reset) * 1000000)::bigint
      FROM pg_stat_wal
      """

  @pg18 @serial
  Scenario: PG18 seals the V2 pg_stat_wal layout
    Given a fresh database on PostgreSQL 18
    When the collector snapshots the segment
    Then section 1_007_002 uses the PG18 pg_stat_wal layout
    And section 1_007_001 is absent from the segment
    And section 1_007_002 stats_reset matches the exact oracle:
      """
      SELECT (EXTRACT(EPOCH FROM stats_reset) * 1000000)::bigint
      FROM pg_stat_wal
      """
