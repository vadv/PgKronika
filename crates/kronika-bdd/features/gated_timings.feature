@web @anomalies
Feature: Timings measured under a disabled GUC read as not collected
  @pg17 @serial
  Scenario: blk_read_time pairs under track_io_timing=off are not collected
    Given a fresh database on PostgreSQL 17
    And the server is reconfigured with:
      """
      ALTER SYSTEM SET track_io_timing = off;
      SELECT pg_reload_conf();
      """
    When the collector ticks for 5 seconds and seals the segment
    Then the web API diffs column blk_read_time of section pg_stat_database as not collected throughout
    And the web API keeps rates for column blks_hit of section pg_stat_database
