Feature: The collector seals OS procfs sections from a fixture /proc tree

  The collector reads procfs through KRONIKA_PROC_ROOT; pointing it at a
  fixture directory makes the assertions host-independent: the fixture
  content is the oracle.

  @pg16 @serial
  Scenario: CPU ticks and misc counters are sealed from a fixture /proc/stat
    Given a fresh database on PostgreSQL 16
    And a fixture proc tree
    And the fixture proc file "stat" contains:
      """
      cpu  100 20 30 400 5 6 7 8 9 10
      cpu0 100 20 30 400 5 6 7 8 9 10
      ctxt 12345
      btime 1700000000
      processes 42
      procs_running 2
      procs_blocked 0
      """
    When the collector snapshots the segment
    Then section 1_102_001 has 2 rows
    And section 1_102_001 cpu_id row -1 has user = 100
    And section 1_102_001 cpu_id row -1 has idle = 400
    And section 1_103_001 ctxt equals 12345
    And section 1_103_001 processes equals 42
