Feature: Wave 3 OS process and cgroup sections use fixture procfs and sysfs trees

  Process and cgroup rows are sealed from KRONIKA_PROC_ROOT and
  KRONIKA_SYS_ROOT fixtures. The fixture roots keep assertions independent from
  the host process table and cgroup mount layout.

  @pg16 @serial
  Scenario: process rows preserve identity, nullable io, and cgroup mapping
    Given a fresh database on PostgreSQL 16
    And a fixture proc tree
    And the fixture proc file "123/stat" contains:
      """
      123 (postgres: check) S 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 -5 16 17 190 204800 12 21 22 23 24 25 26 27 28 29 30 31 32 33 15 2 7 8 9 10 11 12 13 14 15
      """
    And the fixture proc file "123/status" contains:
      """
      Name:	postgres
      Uid:	1000	1001	1002	1003
      Gid:	2000	2001	2002	2003
      VmData:	10 kB
      VmStk:	11 kB
      VmLib:	12 kB
      VmSwap:	13 kB
      VmLck:	14 kB
      VmPTE:	15 kB
      VmPeak:	16 kB
      VmHWM:	17 kB
      Threads:	3
      FDSize:	64
      voluntary_ctxt_switches:	20
      nonvoluntary_ctxt_switches:	21
      """
    And the fixture proc file "123/io" contains:
      """
      rchar: 1
      wchar: 2
      syscr: 3
      syscw: 4
      read_bytes: 5
      write_bytes: 6
      cancelled_write_bytes: 7
      """
    And the fixture proc file "123/schedstat" contains:
      """
      111 2222 3
      """
    And the fixture proc file "123/comm" contains:
      """
      postgres-check
      """
    And the fixture proc file "123/cmdline" contains:
      """
      postgres --check
      """
    And the fixture proc file "123/cgroup" contains:
      """
      0::/kubepods/pod-a/container-123
      """
    And the fixture proc file "124/stat" contains:
      """
      124 (noio) S 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 -5 16 17 190 204800 12 21 22 23 24 25 26 27 28 29 30 31 32 33 15 2 7 8 9 10 11 12 13 14 15
      """
    And the fixture proc file "124/status" contains:
      """
      Uid:	1000	1000	1000	1000
      Gid:	2000	2000	2000	2000
      Threads:	1
      FDSize:	32
      """
    And the fixture proc file "124/cgroup" contains:
      """
      4:pids:/docker/noio
      """
    When the collector snapshots the segment
    Then section os_process has a row with pid = 123:
      | ppid                  | 1                |
      | uid                   | 1000             |
      | euid                  | 1001             |
      | gid                   | 2000             |
      | egid                  | 2001             |
      | state                 | 83               |
      | num_threads           | 16               |
      | tty                   | 4                |
      | comm                  | postgres-check   |
      | cmdline               | postgres --check |
      | utime                 | 11               |
      | stime                 | 12               |
      | nice                  | -5               |
      | prio                  | 15               |
      | rtprio                | 7                |
      | policy                | 8                |
      | curcpu                | 2                |
      | rundelay_ns           | 2222             |
      | blkdelay_ticks        | 9                |
      | nvcsw                 | 20               |
      | nivcsw                | 21               |
      | minflt                | 7                |
      | majflt                | 9                |
      | vmem_kb               | 200              |
      | vswap_kb              | 13               |
      | syscr                 | 3                |
      | syscw                 | 4                |
      | rchar                 | 1                |
      | wchar                 | 2                |
      | read_bytes            | 5                |
      | write_bytes           | 6                |
      | cancelled_write_bytes | 7                |
      | exit_signal           | 15               |
      | scope                 | 0                |
    And section os_process has a row with pid = 124:
      | comm       | noio |
      | syscr      | null |
      | read_bytes | null |
      | cmdline    | null |
    And section os_process_status has a row with pid = 123:
      | vm_data                     | 10 |
      | vm_stk                      | 11 |
      | vm_lib                      | 12 |
      | vm_lck                      | 14 |
      | vm_pte                      | 15 |
      | vm_peak                     | 16 |
      | vm_hwm                      | 17 |
      | threads                     | 3  |
      | fdsize                      | 64 |
      | voluntary_ctxt_switches     | 20 |
      | nonvoluntary_ctxt_switches  | 21 |
      | scope                       | 0  |
    And section os_cgroup_mapping has a row with pid = 123:
      | cgroup_path | /kubepods/pod-a/container-123 |
      | scope       | 0                             |
    And section os_cgroup_mapping has a row with pid = 124:
      | cgroup_path | /docker/noio |

  @pg16 @serial
  Scenario: cgroup v2 cpu, memory, io, and pids are sealed from sysfs
    Given a fresh database on PostgreSQL 16
    And a fixture proc tree
    And the fixture sys file "fs/cgroup/cgroup.controllers" contains:
      """
      cpu memory io pids
      """
    And the fixture sys file "fs/cgroup/kubepods/pod-a/cpu.stat" contains:
      """
      usage_usec 1000
      user_usec 700
      system_usec 300
      nr_throttled 2
      throttled_usec 50
      """
    And the fixture sys file "fs/cgroup/kubepods/pod-a/cpu.max" contains:
      """
      max 100000
      """
    And the fixture sys file "fs/cgroup/kubepods/pod-a/memory.current" contains:
      """
      4096
      """
    And the fixture sys file "fs/cgroup/kubepods/pod-a/memory.max" contains:
      """
      max
      """
    And the fixture sys file "fs/cgroup/kubepods/pod-a/memory.stat" contains:
      """
      anon 1000
      file 2000
      kernel 300
      slab 120
      """
    And the fixture sys file "fs/cgroup/kubepods/pod-a/memory.events" contains:
      """
      low 1
      high 2
      max 3
      oom 4
      oom_kill 5
      """
    And the fixture sys file "fs/cgroup/kubepods/pod-a/io.stat" contains:
      """
      8:0 rbytes=10 wbytes=20 rios=3 wios=4
      """
    And the fixture sys file "fs/cgroup/kubepods/pod-a/pids.current" contains:
      """
      7
      """
    And the fixture sys file "fs/cgroup/kubepods/pod-a/pids.max" contains:
      """
      100
      """
    When the collector snapshots the segment
    Then section os_cgroup_cpu has a row with cgroup_path = "/kubepods/pod-a":
      | usage_usec     | 1000   |
      | user_usec      | 700    |
      | system_usec    | 300    |
      | throttled_usec | 50     |
      | nr_throttled   | 2      |
      | quota_usec     | -1     |
      | period_usec    | 100000 |
      | scope          | 0      |
    And section os_cgroup_memory has a row with cgroup_path = "/kubepods/pod-a":
      | current     | 4096 |
      | max         | null |
      | anon        | 1000 |
      | file        | 2000 |
      | kernel      | 300  |
      | slab        | 120  |
      | low_events  | 1    |
      | high_events | 2    |
      | max_events  | 3    |
      | oom_events  | 4    |
      | oom_kill    | 5    |
      | scope       | 0    |
    And section os_cgroup_io has a row with cgroup_path = "/kubepods/pod-a" and major = 8 and minor = 0:
      | rbytes | 10 |
      | wbytes | 20 |
      | rios   | 3  |
      | wios   | 4  |
      | scope  | 0  |
    And section os_cgroup_pids has a row with cgroup_path = "/kubepods/pod-a":
      | current | 7   |
      | max     | 100 |
      | scope   | 0   |

  @pg16 @serial
  Scenario: cgroup v1 controllers are normalized into the same sections
    Given a fresh database on PostgreSQL 16
    And a fixture proc tree
    And the fixture sys file "fs/cgroup/cpu,cpuacct/docker/abc/cpuacct.usage" contains:
      """
      123456000
      """
    And the fixture sys file "fs/cgroup/cpu,cpuacct/docker/abc/cpu.cfs_quota_us" contains:
      """
      50000
      """
    And the fixture sys file "fs/cgroup/cpu,cpuacct/docker/abc/cpu.cfs_period_us" contains:
      """
      100000
      """
    And the fixture sys file "fs/cgroup/cpu,cpuacct/docker/abc/cpu.stat" contains:
      """
      nr_throttled 2
      throttled_time 7000
      """
    And the fixture sys file "fs/cgroup/memory/docker/abc/memory.usage_in_bytes" contains:
      """
      4096
      """
    And the fixture sys file "fs/cgroup/memory/docker/abc/memory.limit_in_bytes" contains:
      """
      8192
      """
    And the fixture sys file "fs/cgroup/memory/docker/abc/memory.stat" contains:
      """
      total_rss 1000
      total_cache 2000
      total_slab 120
      total_kernel_stack 30
      """
    And the fixture sys file "fs/cgroup/memory/docker/abc/memory.failcnt" contains:
      """
      6
      """
    And the fixture sys file "fs/cgroup/pids/docker/abc/pids.current" contains:
      """
      9
      """
    And the fixture sys file "fs/cgroup/pids/docker/abc/pids.max" contains:
      """
      max
      """
    And the fixture sys file "fs/cgroup/blkio/docker/abc/blkio.throttle.io_service_bytes" contains:
      """
      8:0 Read 4096
      8:0 Write 8192
      8:0 Total 12288
      """
    And the fixture sys file "fs/cgroup/blkio/docker/abc/blkio.throttle.io_serviced" contains:
      """
      8:0 Read 4
      8:0 Write 8
      8:0 Total 12
      """
    When the collector snapshots the segment
    Then section os_cgroup_cpu has a row with cgroup_path = "/docker/abc":
      | usage_usec     | 123456 |
      | throttled_usec | 7      |
      | nr_throttled   | 2      |
      | quota_usec     | 50000  |
      | period_usec    | 100000 |
      | scope          | 0      |
    And section os_cgroup_memory has a row with cgroup_path = "/docker/abc":
      | current    | 4096 |
      | max        | 8192 |
      | anon       | 1000 |
      | file       | 2000 |
      | kernel     | 150  |
      | slab       | 120  |
      | max_events | 6    |
      | scope      | 0    |
    And section os_cgroup_io has a row with cgroup_path = "/docker/abc" and major = 8 and minor = 0:
      | rbytes | 4096 |
      | wbytes | 8192 |
      | rios   | 4    |
      | wios   | 8    |
      | scope  | 0    |
    And section os_cgroup_pids has a row with cgroup_path = "/docker/abc":
      | current | 9    |
      | max     | null |
      | scope   | 0    |
