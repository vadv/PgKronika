Feature: Collector seals the replication instance status singleton
  Type 1_015_001 is one instance-wide row: recovery role, timeline, and WAL
  positions. The matrix runs standalone primaries without replicas, so the
  recorded row must have the primary shape: not in recovery, zero streaming
  replicas, and every standby/receiver column NULL — replay_lag_s included,
  because 0 is reserved for a standby whose receive and replay LSN are known
  and equal. current_wal_lsn only advances, so it is checked as a window:
  an oracle read taken before the snapshot is the floor, a read taken after
  it is the ceiling, and the recorded byte offset must lie between them.

  @pg17
  Scenario: a standalone primary seals one replication-instance row
    Given a fresh database on PostgreSQL 17
    And the window floor for section 1_015_001 current_wal_lsn is captured as:
      """
      SELECT pg_wal_lsn_diff(pg_current_wal_lsn(), '0/0')::int8
      """
    When the collector snapshots the segment
    Then section 1_015_001 has exactly one row:
      | is_in_recovery         | false |
      | streaming_replicas     | 0     |
      | replay_lag_s           | null  |
      | standby_receive_lsn    | null  |
      | standby_replay_lsn     | null  |
      | standby_last_replay_at | null  |
      | wal_receiver_status    | null  |
      | sender_host            | null  |
      | sender_port            | null  |
      | slot_name              | null  |
      | latest_end_lsn         | null  |
      | latest_end_time        | null  |
      | received_tli           | null  |
    And section 1_015_001 is_in_recovery matches the exact oracle:
      """
      SELECT pg_is_in_recovery()
      """
    And section 1_015_001 streaming_replicas matches the exact oracle:
      """
      SELECT count(*) FILTER (WHERE state = 'streaming')::int4
      FROM pg_stat_replication
      """
    And section 1_015_001 timeline_id matches the exact oracle:
      """
      SELECT (pg_control_checkpoint()).timeline_id::int4
      """
    And section 1_015_001 current_wal_lsn matches the window oracle up to:
      """
      SELECT pg_wal_lsn_diff(pg_current_wal_lsn(), '0/0')::int8
      """
