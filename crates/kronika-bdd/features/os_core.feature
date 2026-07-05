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
    And section 1_102_001 cpu_id row -1 has scope = 0
    And section 1_103_001 ctxt equals 12345
    And section 1_103_001 processes equals 42
    And section 1_103_001 scope equals 0

  @pg16 @serial
  Scenario: Memory, load, vmstat, and PSI are sealed from fixture proc files
    Given a fresh database on PostgreSQL 16
    And a fixture proc tree
    And the fixture proc file "meminfo" contains:
      """
      MemTotal:        2048 kB
      MemFree:         1024 kB
      MemAvailable:   1536 kB
      Buffers:           64 kB
      Cached:           512 kB
      Slab:             256 kB
      SReclaimable:     128 kB
      SUnreclaim:       128 kB
      SwapTotal:       4096 kB
      SwapFree:        3072 kB
      Dirty:              7 kB
      Writeback:          8 kB
      """
    And the fixture proc file "loadavg" contains:
      """
      1.25 0.50 0.25 4/120 99
      """
    And the fixture proc file "vmstat" contains:
      """
      pgpgin 111
      pgpgout 222
      pswpin 3
      pswpout 4
      pgfault 555
      pgmajfault 6
      pgsteal_kswapd 7
      pgsteal_direct 8
      pgscan_kswapd 9
      pgscan_direct 10
      oom_kill 2
      """
    And the fixture proc file "pressure/cpu" contains:
      """
      some avg10=0.10 avg60=0.05 avg300=0.02 total=1000
      """
    And the fixture proc file "pressure/memory" contains:
      """
      some avg10=1.50 avg60=0.80 avg300=0.30 total=2000
      full avg10=0.20 avg60=0.10 avg300=0.05 total=3000
      """
    And the fixture proc file "pressure/io" contains:
      """
      some avg10=0.50 avg60=0.25 avg300=0.10 total=4000
      full avg10=0.05 avg60=0.02 avg300=0.01 total=5000
      """
    When the collector snapshots the segment
    Then section 1_104_001 mem_total equals 2048
    And section 1_104_001 s_reclaimable equals 128
    And section 1_104_001 s_unreclaim equals 128
    And section 1_104_001 scope equals 0
    And section 1_105_001 load1 equals 1.25
    And section 1_105_001 running equals 4
    And section 1_105_001 scope equals 0
    And section 1_106_001 pgpgin equals 111
    And section 1_106_001 oom_kill equals 2
    And section 1_106_001 scope equals 0
    And section 1_107_001 has 3 rows
    And section 1_107_001 resource row 0 has some_total = 1000
    And section 1_107_001 resource row 0 has full_total = null
    And section 1_107_001 resource row 1 has full_total = 3000
    And section 1_107_001 resource row 2 has full_total = 5000
