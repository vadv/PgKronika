Feature: Collector reads pg_stat_database
  The collector seals pg_stat_database rows using the layout selected by the
  PostgreSQL major version. The shared-objects row keeps datid=0 and a null
  datname; database rows keep dictionary-backed names and catalog fields.

  Scenario: matrix clusters seal pg_stat_database catalog fields
    Given the PostgreSQL matrix is booted
    Then each matrix cluster seals pg_stat_database rows with catalog fields and dictionary-backed names
