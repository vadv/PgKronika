Feature: Coverage rows (1_023_001) describe truncated top-N sources
  A top-N section without coverage reads as complete data. When a source
  reports more rows than the collector wrote, the segment gets one 1_023_001
  row naming the source section, how many rows the source reported, how many
  were written, the configured limit, and the reason. A source that fits its
  limits gets no coverage row: an empty section means no truncation was
  recorded.

  @pg16 @serial
  Scenario: a table count above the per-axis limit records truncation
    Given a fresh database on PostgreSQL 16
    And the collector runs with env "KRONIKA_PG_MAX_TABLES" = "5"
    And a database seeded with:
      """
      DO $$
      BEGIN
        FOR i IN 1..40 LOOP
          EXECUTE format('CREATE TABLE kronika_cov_%s(id int)', i);
        END LOOP;
      END $$;
      """
    When the collector snapshots the segment
    Then section 1_023_001 has a row with source_type_id = 1013003:
      | total        | 40   |
      | unknown_total | false |
      | max_n        | 5    |
      | reason       | 0    |
      | cutoff_value | null |
    And section 1_023_001 total matches the exact oracle:
      """
      SELECT count(*)::int8 FROM pg_stat_user_tables
      """

  @pg15 @serial
  Scenario: sources under their limits write no coverage rows
    Given a fresh database on PostgreSQL 15
    And a database seeded with:
      """
      CREATE TABLE kronika_cov_single(id int);
      """
    When the collector snapshots the segment
    Then section 1_023_001 is absent from the segment
