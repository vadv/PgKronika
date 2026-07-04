Feature: The scheduler paces sources by their own intervals
  The collector ticks on an internal timer (KRONIKA_INTERVAL_S) and each tick
  reads only the sources whose interval elapsed. The first tick after start
  reads everything, so the first segment is self-contained. SIGUSR2 is a
  forced tick: it reads every source regardless of intervals. Sealing a
  segment re-arms the per-segment service sources (settings, instance,
  reset), so every sealed file carries them — the sources that stay skipped
  between their intervals are the ordinary paced ones, like the table
  statistics below. An interval equal to the tick races timer jitter;
  every-tick sources use interval 0. The open segment lives as a journal
  file in the output directory: a restarted collector seals whatever
  windows it finds there before collecting, and a journal that hits its
  own byte cap seals the open segment early instead of failing. The sized
  pool sources (statements, tables, indexes) run under a per-cycle
  database-time budget: a source over the budget moves to the next tick,
  where it runs unconditionally — deferral never becomes starvation.
  Triggers read the rows already collected: lock waiters or active-backend
  pressure accelerate the activity pace, replication lag or retained WAL
  accelerate the replication pace, and the pace relaxes when the condition
  clears.

  @pg17 @serial
  Scenario: later ticks skip sources that are not due
    Given a fresh database on PostgreSQL 17
    And a database seeded with:
      """
      CREATE TABLE kronika_sched_probe(id int);
      """
    And the collector runs with env "KRONIKA_INTERVAL_S" = "1"
    And the collector runs with env "KRONIKA_PG_ACTIVITY_INTERVAL_S" = "0"
    And the collector runs with env "KRONIKA_PG_SETTINGS_INTERVAL_S" = "3600"
    And the collector runs with env "KRONIKA_PG_TABLES_INTERVAL_S" = "3600"
    And the collector runs with env "KRONIKA_SEGMENT_MAX_BYTES" = "1"
    When the collector runs on its own timer until 2 segments are sealed
    Then timer segment 1 has section 1_019_001
    And timer segment 1 has section 1_013_003
    And timer segment 1 has section 1_001_003
    And timer segment 2 has section 1_001_003
    And timer segment 2 has section 1_019_001
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
    And the collector runs with env "KRONIKA_PG_ACTIVITY_INTERVAL_S" = "0"
    And the collector runs with env "KRONIKA_SEGMENT_MAX_AGE_S" = "3"
    When the collector runs on its own timer until 1 segment is sealed
    Then timer segment 1 section 1_001_003 contains at least 2 snapshots
    And timer segment 1 has section 1_019_001

  @pg15 @serial
  Scenario: max age seals even when every due source stays empty
    Given a fresh database on PostgreSQL 15
    And the collector runs with env "KRONIKA_INTERVAL_S" = "1"
    And the collector runs with env "KRONIKA_PG_ACTIVITY_INTERVAL_S" = "3600"
    And the collector runs with env "KRONIKA_PG_PROGRESS_VACUUM_INTERVAL_S" = "0"
    And the collector runs with env "KRONIKA_SEGMENT_MAX_AGE_S" = "3"
    When the collector runs on its own timer until 1 segment is sealed
    Then timer segment 1 has section 1_001_003

  @pg16 @serial
  Scenario: a full journal seals the open segment instead of wedging
    Given a fresh database on PostgreSQL 16
    And the collector runs with env "KRONIKA_INTERVAL_S" = "1"
    And the collector runs with env "KRONIKA_PG_ACTIVITY_INTERVAL_S" = "0"
    And the collector runs with env "KRONIKA_JOURNAL_MAX_BYTES" = "1"
    And the collector runs with env "KRONIKA_SEGMENT_MAX_AGE_S" = "3600"
    When the collector runs on its own timer until 2 segments are sealed
    Then timer segment 1 has section 1_001_003
    And timer segment 2 has section 1_001_003

  @pg17 @serial
  Scenario: windows on disk survive a collector restart
    Given a fresh database on PostgreSQL 17
    And the collector runs with env "KRONIKA_INTERVAL_S" = "1"
    And the collector runs with env "KRONIKA_PG_ACTIVITY_INTERVAL_S" = "0"
    And the collector runs with env "KRONIKA_SEGMENT_MAX_AGE_S" = "3600"
    When the collector is killed mid-segment and restarted
    Then timer segment 1 has section 1_001_003
    And timer segment 1 has section 1_019_001

  @pg15 @serial
  Scenario: a lock wait accelerates the activity pace
    Given a fresh database on PostgreSQL 15
    And a database seeded with:
      """
      CREATE TABLE kronika_pace_probe(id int);
      """
    And session "H" runs and holds its transaction open:
      """
      BEGIN;
      LOCK TABLE kronika_pace_probe IN ACCESS EXCLUSIVE MODE;
      """
    And session "W" runs and blocks:
      """
      SELECT count(*) FROM kronika_pace_probe
      """
    And the collector runs with env "KRONIKA_INTERVAL_S" = "1"
    And the collector runs with env "KRONIKA_PG_ACTIVITY_FAST_INTERVAL_S" = "0"
    And the collector runs with env "KRONIKA_SEGMENT_MAX_AGE_S" = "4"
    When the collector runs on its own timer until 1 segment is sealed
    Then timer segment 1 section 1_001_003 contains at least 3 snapshots

  @pg17 @serial
  Scenario: the cycle budget defers sized sources and repays them next tick
    Given a fresh database on PostgreSQL 17
    And a database seeded with:
      """
      CREATE TABLE kronika_budget_probe(id int);
      """
    And the collector runs with env "KRONIKA_INTERVAL_S" = "1"
    And the collector runs with env "KRONIKA_PG_ACTIVITY_INTERVAL_S" = "0"
    And the collector runs with env "KRONIKA_CYCLE_DB_BUDGET_MS" = "1"
    And the collector runs with env "KRONIKA_SEGMENT_MAX_BYTES" = "1"
    When the collector runs on its own timer until 2 segments are sealed
    Then timer segment 1 has section 1_001_003
    And timer segment 1 has section 1_019_001
    And timer segment 1 is missing section 1_013_003
    And timer segment 2 has section 1_013_003

  @pg15 @serial
  Scenario: a one-byte size cap seals each timer tick
    Given a fresh database on PostgreSQL 15
    And the collector runs with env "KRONIKA_INTERVAL_S" = "1"
    And the collector runs with env "KRONIKA_PG_ACTIVITY_INTERVAL_S" = "0"
    And the collector runs with env "KRONIKA_SEGMENT_MAX_BYTES" = "1"
    When the collector runs on its own timer until 2 segments are sealed
    Then timer segment 1 has section 1_001_003
    And timer segment 2 has section 1_001_003
