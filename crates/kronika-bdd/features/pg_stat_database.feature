Feature: Collector reads pg_stat_database
  The source-pg collector reads pg_stat_database into the version's layout
  (type 1_005_003 on PG 14-17, type 1_005_004 on PG 18) and seals it. The
  scenario reads the rows back, checks the shared-objects row (datid=0, null
  datname), and resolves a real database's datname through the dictionary.

  Scenario: every version seals a readable pg_stat_database section
    Given the PostgreSQL matrix is booted
    Then every version seals a segment whose pg_stat_database rows resolve through the dictionary
