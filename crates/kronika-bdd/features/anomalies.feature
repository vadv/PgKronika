@web @anomalies
Feature: The anomalies endpoint highlights a load spike between snapshots
  When a plateau of transaction load hits a calm baseline, /v1/anomalies
  must report an episode for the affected series, pointing up, with the
  episode interval covering the load.

  @pg17 @serial
  Scenario: a commit-rate plateau at the period's end surfaces as an episode
    Given a fresh database on PostgreSQL 17
    When the collector ticks for 25 calm seconds, carries 60 transactions per second for 6 seconds, and seals the segment
    Then the web API reports an anomaly episode in section pg_stat_database column xact_commit at the period's end
