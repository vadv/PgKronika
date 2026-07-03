Feature: The scheduler paces sources by their own intervals
  The collector ticks on an internal timer (KRONIKA_INTERVAL_S) and each tick
  reads only the sources whose interval elapsed. The first tick after start
  reads everything, so the first segment is self-contained. SIGUSR2 is a
  forced tick: it reads every source regardless of intervals.

  @pg17 @serial
  Scenario: later ticks skip sources that are not due
    Given a fresh database on PostgreSQL 17
    And a database seeded with:
      """
      CREATE TABLE kronika_sched_probe(id int);
      """
    And the collector runs with env "KRONIKA_INTERVAL_S" = "1"
    And the collector runs with env "KRONIKA_PG_ACTIVITY_INTERVAL_S" = "1"
    And the collector runs with env "KRONIKA_PG_SETTINGS_INTERVAL_S" = "3600"
    And the collector runs with env "KRONIKA_PG_TABLES_INTERVAL_S" = "3600"
    And the collector runs with env "KRONIKA_SEGMENT_MAX_BYTES" = "1"
    When the collector runs on its own timer until 2 segments are sealed
    Then timer segment 1 has section 1_019_001
    And timer segment 1 has section 1_013_003
    And timer segment 1 has section 1_001_003
    And timer segment 2 has section 1_001_003
    And timer segment 2 is missing section 1_019_001
    And timer segment 2 is missing section 1_013_003

  @pg16 @serial
  Scenario: a signal reads every source and seals immediately
    Given a fresh database on PostgreSQL 16
    And the collector runs with env "KRONIKA_PG_SETTINGS_INTERVAL_S" = "3600"
    And the collector runs with env "KRONIKA_PG_TABLES_INTERVAL_S" = "3600"
    When the collector snapshots the segment
    Then section 1_019_001 name matches the exact oracle:
      """
      SELECT name FROM pg_settings
      """
    And section 1_021_001 pg_version_num matches the exact oracle:
      """
      SELECT current_setting('server_version_num')::int4
      """

  @pg16 @serial
  Scenario: a segment seals when its max age expires
    Given a fresh database on PostgreSQL 16
    And the collector runs with env "KRONIKA_INTERVAL_S" = "1"
    And the collector runs with env "KRONIKA_PG_ACTIVITY_INTERVAL_S" = "1"
    And the collector runs with env "KRONIKA_SEGMENT_MAX_AGE_S" = "3"
    When the collector runs on its own timer until 1 segment is sealed
    Then timer segment 1 section 1_001_003 contains at least 2 snapshots
    And timer segment 1 has section 1_019_001

  @pg15 @serial
  Scenario: a one-byte size cap seals each timer tick
    Given a fresh database on PostgreSQL 15
    And the collector runs with env "KRONIKA_INTERVAL_S" = "1"
    And the collector runs with env "KRONIKA_PG_ACTIVITY_INTERVAL_S" = "1"
    And the collector runs with env "KRONIKA_SEGMENT_MAX_BYTES" = "1"
    When the collector runs on its own timer until 2 segments are sealed
    Then timer segment 1 has section 1_001_003
    And timer segment 2 has section 1_001_003
