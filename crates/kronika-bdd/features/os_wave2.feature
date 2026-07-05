Feature: Wave 2 OS sections use fixture /proc and /sys trees

  The collector reads diskstats, net/dev, net/snmp, net/netstat,
  self/mountinfo, and topology through KRONIKA_PROC_ROOT / KRONIKA_SYS_ROOT.
  Fixture roots make assertions independent from the host kernel state.

  @pg16 @serial
  Scenario: диски снимаются из фикстурного /proc/diskstats
    Given a fresh database on PostgreSQL 16
    And a fixture proc tree
    And the fixture proc file "diskstats" contains:
      """
         8       1 sda1 500 10 24000 80 300 20 12000 150 2 900 1100 5 0 200 30 4 15
         9       9 md0 100 0 8000 40 50 0 4000 20 0 200 300
      """
    When the collector snapshots the segment
    Then section 1_108_001 has 2 rows
    And section 1_108_001 major 8 minor 1 has reads = 500
    And section 1_108_001 major 8 minor 1 has write_sectors = 12000
    And section 1_108_001 major 8 minor 1 has io_in_progress = 2
    And section 1_108_001 major 9 minor 9 has reads = 100
    And section 1_108_001 major 9 minor 9 has write_sectors = 4000

  @pg16 @serial
  Scenario: legacy diskstats с 14 полями пишет discards = NULL
    Given a fresh database on PostgreSQL 16
    And a fixture proc tree
    And the fixture proc file "diskstats" contains:
      """
       259       0 nvme0n1 1 0 8 2 3 0 24 4 0 6 6
      """
    When the collector snapshots the segment
    Then section 1_108_001 has 1 rows
    And section 1_108_001 major 259 minor 0 has discards = null

  @pg16 @serial
  Scenario: сеть снимается из фикстурных /proc/net/*
    Given a fresh database on PostgreSQL 16
    And a fixture proc tree
    And the fixture proc file "net/dev" contains:
      """
      Inter-|   Receive                                                |  Transmit
       face |bytes    packets errs drop fifo frame compressed multicast|bytes    packets errs drop  fifo colls carrier compressed
          lo:       0       0    0    0    0     0          0         0        0       0    0    0     0     0       0          0
        eth0: 9000000    8000   10   20   30    40         50        60  3000000    2500   11   22   33    44      55         66
      """
    And the fixture proc file "net/snmp" contains:
      """
      Tcp: ActiveOpens PassiveOpens AttemptFails EstabResets InSegs OutSegs RetransSegs InErrs OutRsts CurrEstab
      Tcp: 100 200 5 3 50000 48000 77 2 9 15
      Udp: InDatagrams NoPorts InErrors OutDatagrams
      Udp: 4000 8 6 3500
      """
    And the fixture proc file "net/netstat" contains:
      """
      TcpExt: ListenOverflows ListenDrops TCPTimeouts TCPFastRetrans TCPSlowStartRetrans TCPOFOQueue TCPSynRetrans
      TcpExt: 10 20 30 40 50 60 70
      """
    When the collector snapshots the segment
    Then section 1_109_001 has 2 rows
    And section 1_110_001 tcp_active_opens equals 100
    And section 1_110_001 tcp_passive_opens equals 200
    And section 1_110_001 tcp_curr_estab equals 15
    And section 1_110_001 udp_in_datagrams equals 4000
    And section 1_110_001 udp_no_ports equals 8
    And section 1_110_001 scope equals 0
    And section 1_111_001 listen_overflows equals 10
    And section 1_111_001 listen_drops equals 20
    And section 1_111_001 tcp_timeouts equals 30
    And section 1_111_001 tcp_fast_retrans equals 40
    And section 1_111_001 tcp_slow_start_retrans equals 50
    And section 1_111_001 tcp_ofo_queue equals 60
    And section 1_111_001 tcp_syn_retrans equals 70
    And section 1_111_001 scope equals 0

  @pg16 @serial
  Scenario: сеть в контейнере имеет scope=pod_net
    Given a fresh database on PostgreSQL 16
    And a fixture proc tree
    And the fixture proc tree is a container
    And the fixture proc file "net/dev" contains:
      """
      Inter-|   Receive                                                |  Transmit
       face |bytes    packets errs drop fifo frame compressed multicast|bytes    packets errs drop  fifo colls carrier compressed
          lo:       0       0    0    0    0     0          0         0        0       0    0    0     0     0       0          0
      """
    When the collector snapshots the segment
    Then section 1_110_001 scope equals 2
    And section 1_111_001 scope equals 2

  @pg16 @serial
  Scenario: диск в поде фильтруется через mountinfo
    Given a fresh database on PostgreSQL 16
    And a fixture proc tree
    And the fixture proc tree is a container
    And the fixture proc file "diskstats" contains:
      """
         8       1 sda1 500 10 24000 80 300 20 12000 150 2 900 1100 5 0 200 30 4 15
       253       0 dm-0  50  0  4000 20  30  0  2000  10 0 100  120 0 0   0  0 0  0
         9       9 md0  100  0  8000 40  50  0  4000  20 0 200  300 0 0   0  0 0  0
      """
    And the fixture proc file "self/mountinfo" contains:
      """
      30 25 8:1 / /data rw,relatime shared:1 - ext4 /dev/sda1 rw
      40 25 253:0 / /etc/hosts rw - ext4 /dev/dm-0 rw
      """
    And the statvfs fixture is "/data=10737418240:5368709120"
    When the collector snapshots the segment
    Then section 1_108_001 has 1 rows
    And section 1_108_001 major 8 minor 1 has reads = 500
    And section 1_108_001 has no row with major 253 minor 0
    And section 1_108_001 has no row with major 9 minor 9
    And section 1_112_001 major 8 minor 1 mount_point resolves to "/data"
    And section 1_112_001 major 8 minor 1 has is_k8s_infra = false
    And section 1_112_001 major 8 minor 1 has total_bytes = 10737418240
    And section 1_112_001 major 8 minor 1 has free_bytes = 5368709120

  @pg16 @serial
  Scenario: btrfs major=0 резолвится через /sys/class/block
    Given a fresh database on PostgreSQL 16
    And a fixture proc tree
    And the fixture proc file "diskstats" contains:
      """
       254      16 vdb1 200 0 16000 50 100 0 8000 30 0 400 500 0 0 0 0 0 0
      """
    And the fixture proc file "self/mountinfo" contains:
      """
      50 1 0:35 / /data rw - btrfs /dev/vdb1 rw
      """
    And the fixture sys file "class/block/vdb1/dev" contains:
      """
      254:16
      """
    And the statvfs fixture is "/data=5000000000:2000000000"
    When the collector snapshots the segment
    Then section 1_112_001 major 254 minor 16 mount_point resolves to "/data"
    And section 1_112_001 major 254 minor 16 has total_bytes = 5000000000
    And section 1_112_001 major 254 minor 16 has free_bytes = 2000000000
