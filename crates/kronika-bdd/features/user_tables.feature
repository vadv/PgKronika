Feature: Collector reads pg_stat_user_tables and pg_stat_user_indexes across databases
  The collector walks the connection pool and seals one pg_stat_user_tables row
  per selected table per database, and one pg_stat_user_indexes row per selected
  index per database. Both layouts follow the PostgreSQL major version. datname,
  schemaname, relname and indexrelname are dictionary-backed, so a row keeps its
  database of origin even when two databases hold an object of the same name. Each
  seeded database carries the probe table's primary key plus a partial expression
  index, so the index scenario also covers buffer counters, size, the pg_index
  flags, scan recency on PG16+, and the SQL-level indexdef truncation.

  Scenario: matrix clusters seal tables from every database with datname separation
    Given the PostgreSQL matrix is booted
    Then each matrix cluster seals pg_stat_user_tables rows from two seeded databases with dictionary-backed names

  Scenario: matrix clusters seal the probe table's indexes from every database
    Given the PostgreSQL matrix is booted
    Then each matrix cluster seals pg_stat_user_indexes rows from two seeded databases with dictionary-backed names
