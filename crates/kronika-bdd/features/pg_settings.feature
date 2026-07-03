Feature: Every segment carries pg_settings (1_019_001)
  The full parameter set of the running server goes into each segment, so a
  reader interprets the other sections without reading configuration from
  older segments. The set of names is compared with `pg_settings`; a value
  changed through ALTER SYSTEM plus reload checks that the section reflects
  the running server, not compiled-in defaults. `pg_settings` reports the
  value and its unit separately, plus source category, config file, and line:
  `work_mem` set to `7539kB` is stored as setting `7539` with unit `kB`.

  @pg15 @serial
  Scenario: the full parameter set with an ALTER SYSTEM change
    Given a fresh database on PostgreSQL 15
    And the server is reconfigured with:
      """
      ALTER SYSTEM SET work_mem = '7539kB';
      SELECT pg_reload_conf();
      """
    When the collector snapshots the segment
    Then section 1_019_001 name matches the exact oracle:
      """
      SELECT name FROM pg_settings
      """
    And section 1_019_001 pg_settings entry "work_mem" has setting = "7539"
    And section 1_019_001 pg_settings entry "work_mem" has unit = "kB"
    And section 1_019_001 pg_settings entry "work_mem" has context = "user"
    And section 1_019_001 pg_settings entry "work_mem" has vartype = "integer"
    And section 1_019_001 pg_settings entry "work_mem" has source = "configuration file"
    And section 1_019_001 pg_settings entry "work_mem" has sourcefile ending with "postgresql.auto.conf"
    And section 1_019_001 pg_settings entry "work_mem" has sourceline > 0
    And section 1_019_001 pg_settings entry "work_mem" has pending_restart = "false"

  @pg17 @serial
  Scenario: a postmaster-context change is recorded as pending restart
    Given a fresh database on PostgreSQL 17
    And the server is reconfigured with:
      """
      ALTER SYSTEM SET shared_buffers = '190MB';
      SELECT pg_reload_conf();
      """
    When the collector snapshots the segment
    Then section 1_019_001 pg_settings entry "shared_buffers" has pending_restart = "true"
    And section 1_019_001 pg_settings entry "shared_buffers" has context = "postmaster"
    And section 1_019_001 name matches the exact oracle:
      """
      SELECT name FROM pg_settings
      """
