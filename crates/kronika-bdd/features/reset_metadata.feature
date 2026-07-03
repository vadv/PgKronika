Feature: Every segment carries the reset context (1_020_001)
  reset_metadata lets a reader distinguish a stats reset from an unexplained
  counter decrease. It records the postmaster start time, the per-view
  stats_reset timestamps exposed by this server major, the versions of the
  statistics extensions collected in the snapshot, and the GUCs that affect
  counter and timing columns. The scenarios compare those fields with
  independent PostgreSQL queries.

  @pg17 @serial
  Scenario: extension resets and interpretation GUCs come from PostgreSQL
    Given a fresh database on PostgreSQL 17
    And a database seeded with:
      """
      CREATE EXTENSION pg_stat_statements;
      CREATE EXTENSION pg_store_plans;
      SELECT pg_stat_statements_reset();
      """
    When the collector snapshots the segment
    Then section 1_020_001 postmaster_start_time matches the exact oracle:
      """
      SELECT (extract(epoch from pg_postmaster_start_time()) * 1e6)::int8
      """
    And section 1_020_001 pg_stat_database_reset_max_at matches the exact oracle:
      """
      SELECT (extract(epoch from max(stats_reset)) * 1e6)::int8 FROM pg_stat_database
      """
    And section 1_020_001 pg_stat_statements_reset_at matches the exact oracle:
      """
      SELECT (extract(epoch from stats_reset) * 1e6)::int8 FROM pg_stat_statements_info
      """
    And section 1_020_001 ext_pg_stat_statements_version matches the exact oracle:
      """
      SELECT extversion FROM pg_extension WHERE extname = 'pg_stat_statements'
      """
    And section 1_020_001 ext_pg_store_plans_version matches the exact oracle:
      """
      SELECT extversion FROM pg_extension WHERE extname = 'pg_store_plans'
      """
    And section 1_020_001 pg_store_plans_reset_at is null
    And section 1_020_001 pg_stat_bgwriter_reset_at matches the exact oracle:
      """
      SELECT (extract(epoch from stats_reset) * 1e6)::int8 FROM pg_stat_bgwriter
      """
    And section 1_020_001 pg_stat_checkpointer_reset_at matches the exact oracle:
      """
      SELECT (extract(epoch from stats_reset) * 1e6)::int8 FROM pg_stat_checkpointer
      """
    And section 1_020_001 pg_stat_wal_reset_at matches the exact oracle:
      """
      SELECT (extract(epoch from stats_reset) * 1e6)::int8 FROM pg_stat_wal
      """
    And section 1_020_001 pg_stat_archiver_reset_at matches the exact oracle:
      """
      SELECT (extract(epoch from stats_reset) * 1e6)::int8 FROM pg_stat_archiver
      """
    And section 1_020_001 pg_stat_io_reset_at matches the exact oracle:
      """
      SELECT (extract(epoch from max(stats_reset)) * 1e6)::int8 FROM pg_stat_io
      """
    And section 1_020_001 compute_query_id matches the exact oracle:
      """
      SELECT current_setting('compute_query_id')
      """
    And section 1_020_001 track_io_timing matches the exact oracle:
      """
      SELECT current_setting('track_io_timing')::bool
      """
    And section 1_020_001 track_wal_io_timing matches the exact oracle:
      """
      SELECT current_setting('track_wal_io_timing')::bool
      """

  @pg15 @serial
  Scenario: the ossc fork exposes its reset time through pg_store_plans_info
    Given a fresh database on PostgreSQL 15
    And a database seeded with:
      """
      CREATE EXTENSION pg_store_plans;
      SELECT pg_store_plans_reset();
      """
    When the collector snapshots the segment
    Then section 1_020_001 pg_store_plans_reset_at matches the exact oracle:
      """
      SELECT (extract(epoch from stats_reset) * 1e6)::int8 FROM pg_store_plans_info
      """
    And section 1_020_001 ext_pg_store_plans_version matches the exact oracle:
      """
      SELECT extversion FROM pg_extension WHERE extname = 'pg_store_plans'
      """
    And section 1_020_001 ext_pg_stat_statements_version is null
    And section 1_020_001 pg_stat_statements_reset_at is null
    And section 1_020_001 pg_stat_checkpointer_reset_at is null
    And section 1_020_001 pg_stat_io_reset_at is null
    And section 1_020_001 postmaster_start_time matches the exact oracle:
      """
      SELECT (extract(epoch from pg_postmaster_start_time()) * 1e6)::int8
      """
    And section 1_020_001 track_wal_io_timing matches the exact oracle:
      """
      SELECT current_setting('track_wal_io_timing')::bool
      """
