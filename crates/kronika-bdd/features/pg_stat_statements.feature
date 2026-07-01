Feature: Collector reads pg_stat_statements
  The collector seals pg_stat_statements rows using the layout selected by the
  installed extension version. The view is instance-wide, so one query returns
  every database's statements. queryid, userid and dbid identify a row, and the
  query text is dictionary-backed.

  Scenario: matrix clusters seal pg_stat_statements rows with dictionary-backed query text
    Given the PostgreSQL matrix is booted
    Then each matrix cluster installs pg_stat_statements and seals rows with dictionary-backed query text
