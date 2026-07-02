Feature: Collector seals the pg_stat_archiver singleton
  pg_stat_archiver is one instance-wide row. On a cluster with archiving off
  nothing is ever archived, so after a shared reset the counters are zero and
  the WAL-name labels are absent. The collected row is checked against an
  independent pg_stat_archiver query.

  @pg17 @serial
  Scenario: a reset archiver seals one zeroed row
    Given a fresh database on PostgreSQL 17
    And a database seeded with:
      """
      SELECT pg_stat_reset_shared('archiver');
      """
    When the collector snapshots the segment
    Then section 1_008_001 has exactly one row:
      | archived_count    | 0    |
      | failed_count      | 0    |
      | last_archived_wal | null |
      | last_failed_wal   | null |
    And section 1_008_001 archived_count matches the exact oracle:
      """
      SELECT archived_count FROM pg_stat_archiver
      """
    And section 1_008_001 failed_count matches the exact oracle:
      """
      SELECT failed_count FROM pg_stat_archiver
      """
