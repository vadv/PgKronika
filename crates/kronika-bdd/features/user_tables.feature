Feature: Collector reads pg_stat_user_tables across databases
  The collector walks the connection pool and seals one pg_stat_user_tables row
  per selected table per database. The layout follows the PostgreSQL major
  version. datname, schemaname and relname are dictionary-backed, so a row keeps
  its database of origin even when two databases hold a table of the same name.

  Scenario: matrix clusters seal tables from every database with datname separation
    Given the PostgreSQL matrix is booted
    Then each matrix cluster seals pg_stat_user_tables rows from two seeded databases with dictionary-backed names
