Feature: Every segment carries the instance fingerprint (1_021_001)
  instance_metadata anchors a segment to one server and one host boot: the
  PostgreSQL version and control-file identifier explain version-dependent
  sections, and the host facts (boot id, boot time, tick rate, page size) let
  OS sections be read without external configuration. PostgreSQL supplies the
  server facts; the host facts are checked against /proc and sysconf reads.

  @pg17 @serial
  Scenario: the segment carries the server and host fingerprint
    Given a fresh database on PostgreSQL 17
    When the collector snapshots the segment
    Then section 1_021_001 pg_version_num matches the exact oracle:
      """
      SELECT current_setting('server_version_num')::int4
      """
    And section 1_021_001 pg_system_identifier matches the exact oracle:
      """
      SELECT system_identifier FROM pg_control_system()
      """
    And section 1_021_001 hostname equals the trimmed content of "/proc/sys/kernel/hostname"
    And section 1_021_001 node_self_id equals the trimmed content of "/proc/sys/kernel/hostname"
    And section 1_021_001 kernel_version equals the trimmed content of "/proc/sys/kernel/osrelease"
    And section 1_021_001 boot_id equals the trimmed content of "/proc/sys/kernel/random/boot_id"
    And section 1_021_001 btime equals the /proc/stat btime in microseconds
    And section 1_021_001 clock_ticks_per_sec equals the local sysconf clock ticks
    And section 1_021_001 page_size_bytes equals the local sysconf page size

  @pg15 @serial
  Scenario: the node id comes from the environment when set
    Given a fresh database on PostgreSQL 15
    And the collector runs with env "KRONIKA_NODE_SELF_ID" = "bdd-node-042"
    When the collector snapshots the segment
    Then section 1_021_001 node_self_id resolves to "bdd-node-042"
    And section 1_021_001 hostname equals the trimmed content of "/proc/sys/kernel/hostname"
    And section 1_021_001 pg_version_num matches the exact oracle:
      """
      SELECT current_setting('server_version_num')::int4
      """
