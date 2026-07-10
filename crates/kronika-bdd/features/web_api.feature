Feature: The web API serves collected sections over HTTP
  The collector output that the direct-decode scenarios check is also served
  through the in-process JSON router. A matching row proves the reader query
  layer and the HTTP serialization agree with the sealed segment, end to end.

  @pg17 @serial
  Scenario: the web API serves the reset archiver row
    Given a fresh database on PostgreSQL 17
    And a database seeded with:
      """
      SELECT pg_stat_reset_shared('archiver');
      """
    When the collector snapshots the segment
    Then the web API serves section pg_stat_archiver with one row:
      | archived_count    | 0    |
      | failed_count      | 0    |
      | last_archived_wal | null |
      | last_failed_wal   | null |
