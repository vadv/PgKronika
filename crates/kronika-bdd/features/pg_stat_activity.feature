Feature: Collector reads pg_stat_activity into section 1_001_003
  The source-pg collector snapshots pg_stat_activity into section 1_001_003
  (layout PG 14-18). The scenario opens a session with a distinctive query
  text, snapshots the segment, and checks the recorded row against concrete
  values resolved through the dictionary.

  @pg17 @serial
  Scenario: an idle-in-transaction session is recorded with state and query resolved through the dictionary
    Given a fresh database on PostgreSQL 17
    And a database seeded with:
      """
      CREATE TABLE kronika_probe(id int primary key);
      """
    And session "S" runs and holds its transaction open:
      """
      BEGIN;
      SELECT 'kronika_bdd_activity_marker' FROM kronika_probe WHERE id = 0;
      """
    When the collector snapshots the segment
    Then section 1_001_003 has a row for session "S":
      | pid   | [S]                         |
      | state | idle in transaction          |
      | query | kronika_bdd_activity_marker |
    And section 1_001_003 pid is present in pg_stat_activity:
      """
      SELECT pid FROM pg_stat_activity
      WHERE state = 'idle in transaction'
      """
