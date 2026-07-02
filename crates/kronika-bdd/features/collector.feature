Feature: Collector reads bgwriter and checkpointer stats
  Section 1_006_001 stores pg_stat_bgwriter on every supported PostgreSQL
  major. PG17 split checkpoint counters into pg_stat_checkpointer, so the BDD
  contract pins both the pre-PG17 nullable layout and the PG17+ layout.

  @pg15 @serial
  Scenario: PG15 seals the pre-PG17 bgwriter/checkpointer layout
    Given a fresh database on PostgreSQL 15
    When the collector snapshots the segment
    Then section 1_006_001 uses the pre-PG17 bgwriter/checkpointer layout
    And section 1_006_001 bgwriter_stats_reset matches the exact oracle:
      """
      SELECT (EXTRACT(EPOCH FROM stats_reset) * 1000000)::bigint
      FROM pg_stat_bgwriter
      """

  @pg16 @serial
  Scenario: PG16 seals the pre-PG17 bgwriter/checkpointer layout
    Given a fresh database on PostgreSQL 16
    When the collector snapshots the segment
    Then section 1_006_001 uses the pre-PG17 bgwriter/checkpointer layout
    And section 1_006_001 bgwriter_stats_reset matches the exact oracle:
      """
      SELECT (EXTRACT(EPOCH FROM stats_reset) * 1000000)::bigint
      FROM pg_stat_bgwriter
      """

  @pg17 @serial
  Scenario: PG17 seals the split bgwriter/checkpointer layout
    Given a fresh database on PostgreSQL 17
    When the collector snapshots the segment
    Then section 1_006_001 uses the PG17+ bgwriter/checkpointer layout
    And section 1_006_001 bgwriter_stats_reset matches the exact oracle:
      """
      SELECT (EXTRACT(EPOCH FROM stats_reset) * 1000000)::bigint
      FROM pg_stat_bgwriter
      """
    And section 1_006_001 checkpointer_stats_reset matches the exact oracle:
      """
      SELECT (EXTRACT(EPOCH FROM stats_reset) * 1000000)::bigint
      FROM pg_stat_checkpointer
      """

  @pg18 @serial
  Scenario: PG18 seals the split bgwriter/checkpointer layout
    Given a fresh database on PostgreSQL 18
    When the collector snapshots the segment
    Then section 1_006_001 uses the PG17+ bgwriter/checkpointer layout
    And section 1_006_001 bgwriter_stats_reset matches the exact oracle:
      """
      SELECT (EXTRACT(EPOCH FROM stats_reset) * 1000000)::bigint
      FROM pg_stat_bgwriter
      """
    And section 1_006_001 checkpointer_stats_reset matches the exact oracle:
      """
      SELECT (EXTRACT(EPOCH FROM stats_reset) * 1000000)::bigint
      FROM pg_stat_checkpointer
      """
