Feature: Collector reads pg_stat_io
  The source-pg collector reads pg_stat_io into the version's layout (type
  1_009_001 on PG 16-17 with op_bytes, type 1_009_002 on PG 18 with per-op byte
  counters). The view does not exist before PG16, so the section is absent there,
  and each version seals only its own layout. The scenario reads the rows back,
  confirms the layout-specific columns and that any stats_reset precedes the
  snapshot, and resolves their backend_type / object / context labels through the
  segment dictionary.

  Scenario: every version handles pg_stat_io per its layout
    Given the PostgreSQL matrix is booted
    Then every version handles pg_stat_io per its layout, resolving labels through the dictionary
