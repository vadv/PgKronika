Feature: Collector reads pg_stat_activity
  The source-pg collector reads pg_stat_activity into the version's layout
  (type 1_001_003 on PostgreSQL 14-18) and seals it alongside the segment
  string dictionary. The scenario verifies typed decode and dictionary
  resolution for the collector backend.

  Scenario: every version seals a readable pg_stat_activity section
    Given the PostgreSQL matrix is booted
    Then every version seals a segment whose pg_stat_activity rows resolve through the dictionary
