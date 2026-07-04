# Класс 1: ОС и cgroup

ОС-источники занимают диапазон `1_100_001` - `1_299_999`.

**Реализовано (Wave 1):** `1_102_001`-`1_107_001` (CPU, stat-прочее, meminfo,
loadavg, vmstat, PSI). Каждая строка несёт колонку `scope` (`u8`:
`0=host, 1=pod, 2=pod_net, 3=container, 4=unknown`); в Wave 1 procfs-core
файлы протекают host-wide в контейнере, поэтому их `scope` = `host`.
Корень procfs переопределяется `KRONIKA_PROC_ROOT` (по умолчанию `/proc`),
период — `KRONIKA_OS_CORE_INTERVAL_S` (по умолчанию 10 с). Остальные ОС-типы
(processes, диск, сеть, cgroup) — следующие волны.

## Сводная таблица

| `type_id` | Источник | Период | Семантика | Сортировка |
|-----------|----------|----------|-----------|------------|
| `1_100_001` | processes, горячий набор | 5 с | `snapshot_full` | `(pid, starttime, ts)` |
| `1_101_001` | `/proc/PID/status`, расширенный набор | 30 с | `snapshot_full` | `(pid, starttime, ts)` |
| `1_102_001` | `/proc/stat`: CPU | базовый шаг | `snapshot_full` | `(cpu_id, ts)` |
| `1_103_001` | `/proc/stat`: прочее | базовый шаг | `snapshot_full` | `(ts)` |
| `1_104_001` | `/proc/meminfo` | базовый шаг | `snapshot_full` | `(ts)` |
| `1_105_001` | `/proc/loadavg` | базовый шаг | `snapshot_full` | `(ts)` |
| `1_106_001` | `/proc/vmstat` | базовый шаг | `snapshot_full` | `(ts)` |
| `1_107_001` | `/proc/pressure/*` | базовый шаг | `snapshot_full` | `(resource, ts)` |
| `1_108_001` | `/proc/diskstats` | базовый шаг | `snapshot_full` | `(major, minor, ts)` |
| `1_109_001` | `/proc/net/dev` | базовый шаг | `snapshot_full` | `(iface, ts)` |
| `1_110_001` | `/proc/net/snmp` | базовый шаг | `snapshot_full` | `(ts)` |
| `1_111_001` | `/proc/net/netstat` | базовый шаг | `snapshot_full` | `(ts)` |
| `1_112_001` | `mountinfo` | сегмент + по изменению | `on_change` | `(major, minor, mount_point)` |
| `1_113_001` | `cpuinfo` / topology | сегмент + по изменению | `on_change` | `(cpu_id)` |
| `1_200_001` | cgroup: process mapping | 30 с | `snapshot_full` | `(pid, starttime, ts)` |
| `1_201_001` | cgroup: cpu | базовый шаг | `snapshot_full` | `(cgroup_path, ts)` |
| `1_202_001` | cgroup: memory | базовый шаг | `snapshot_full` | `(cgroup_path, ts)` |
| `1_203_001` | cgroup: io | базовый шаг | `snapshot_full` | `(cgroup_path, major, minor, ts)` |
| `1_204_001` | cgroup: pids | базовый шаг | `snapshot_full` | `(cgroup_path, ts)` |

## Бюджеты и права доступа

ОС-коллектор должен иметь бюджеты на число сущностей и системные вызовы.
Минимальные настройки:

- максимальное число PID за один проход сбора;
- максимальное число cgroup за один проход сбора;
- максимальное число пар `(cgroup, device)` для `1_203_001`;
- максимальное время чтения `/proc` и `/sys` за один проход сбора;
- режим деградации: пропуск низкоприоритетных источников, досрочная запись
  буфера, событие `collector_gap` или событие о проблеме с правами либо
  тайм-аутом.

Если лимит сработал, коллектор не должен молча выдавать частичный снимок как
полный. Для частичного сбора нужна диагностическая метрика/событие и, где
применимо, coverage.

Права доступа:

| Источник | Риск | Поведение |
|----------|------|-----------|
| `/proc/PID/io` | чужой uid, `hidepid`, Yama, SELinux/AppArmor, container PID namespace | недоступные io-колонки пишутся `NULL`, не `0` |
| `/proc/PID/cmdline` | процесс исчез или доступ закрыт | `cmdline = NULL`, строка процесса может остаться |
| `/sys/fs/cgroup` | rootless/container mode, отсутствующий контроллер, нестандартный mount layout | отсутствующие поля `NULL`, отсутствующий контроллер не имитируется нулями |
| обход логов, cgroup и `/proc` | тайм-аут или отказ в доступе | событие/метрика коллектора с указанием источника |

## `1_100_001` processes, горячий набор

Самый объёмный системный тип: все PID на каждом регулярном сборе. Источники:
`/proc/PID/stat`, `/proc/PID/io`, `/proc/PID/schedstat`, `/proc/PID/comm`,
`/proc/PID/cmdline`.

Ключ сущности — `(pid, starttime)`. `starttime` нужен, чтобы отделять повторно
использованный PID.

Контейнерная особенность: чтение `/proc/PID/io` для процессов другого uid может
требовать `setfsuid` без `CAP_SYS_PTRACE`. Если io-данные недоступны,
соответствующие колонки пишутся как `NULL`, а не как нули.

```text
ts                      ts    T
pid                     i32   L
starttime               ts    L   // btime + jiffies
ppid                    i32   L
uid                     u32   L
euid                    u32   L
gid                     u32   L
egid                    u32   L
state                   u8    L   // R S D Z T ...
num_threads             u32   G
tty                     u16   L
comm                    str   L
cmdline                 str?  L
utime                   i64   C   // ticks
stime                   i64   C   // ticks
nice                    i8    L
prio                    i16   L
rtprio                  i16   L
policy                  u8    L
curcpu                  i32   G
rundelay_ns             i64   C
blkdelay_ticks          i64   C
nvcsw                   i64   C
nivcsw                  i64   C
minflt                  i64   C
majflt                  i64   C
vmem_kb                 i64   G
rmem_kb                 i64   G
vswap_kb                i64   G
syscr                   i64?  C
syscw                   i64?  C
rchar                   i64?  C
wchar                   i64?  C
read_bytes              i64?  C
write_bytes             i64?  C
cancelled_write_bytes   i64?  C
```

`utime` и `stime` хранятся в единицах планировщика (`ticks`). Значение `hz`
должно храниться в
`instance_metadata`; код чтения не должен брать его из внешней конфигурации.

## `1_101_001` `/proc/PID/status`, расширенный набор

Отделён от `1_100_001`, потому что тяжелее парсится и нужен реже.

```text
ts                          ts    T
pid                         i32   L
starttime                   ts    L
vm_data                     i64   G
vm_stk                      i64   G
vm_lib                      i64   G
vm_lck                      i64   G
vm_pte                      i64   G
vm_peak                     i64   G
vm_hwm                      i64   G
threads                     u32   G
fdsize                      u32   G
voluntary_ctxt_switches     i64   C
nonvoluntary_ctxt_switches  i64   C
```

Кандидаты для будущих версий: число fd, `wchan`, `oom_score`.

## `1_102_001` `/proc/stat`: CPU

```text
ts          ts   T
cpu_id      i32? L   // NULL = агрегатная строка "cpu"
user        i64  C
nice        i64  C
system      i64  C
idle        i64  C
iowait      i64  C
irq         i64  C
softirq     i64  C
steal       i64  C
guest       i64  C
guest_nice  i64  C
```

Значения хранятся в единицах планировщика (`ticks`).

## `1_103_001` `/proc/stat`: прочее

```text
ts             ts   T
ctxt           i64  C
processes      i64  C   // forks
procs_running  i32  G
procs_blocked  i32  G
btime          ts   L
```

`procs_blocked` полезен как быстрый индикатор D-state и возможного IO-затыка.

## `1_104_001` `/proc/meminfo`

Новая версия пишет широкий набор необязательных ключей. Ключи, отсутствующие в ядре,
пишутся как `NULL`. Значения в КБ, кроме `HugePages_*`, где исходное значение
измеряется в штуках.

```text
ts                ts   T
mem_total         i64  G
mem_free          i64  G
mem_available     i64  G
buffers           i64  G
cached            i64  G
swap_cached       i64  G
active            i64  G
inactive          i64  G
active_anon       i64  G
inactive_anon     i64  G
active_file       i64  G
inactive_file     i64  G
unevictable       i64  G
mlocked           i64  G
swap_total        i64  G
swap_free         i64  G
dirty             i64  G
writeback         i64  G
anon_pages        i64  G
mapped            i64  G
shmem             i64  G
kreclaimable      i64  G
slab              i64  G
s_reclaimable     i64  G
s_unreclaim       i64  G
kernel_stack      i64  G
page_tables       i64  G
commit_limit      i64  G
committed_as      i64  G
vmalloc_total     i64  G
vmalloc_used      i64  G
hugepages_total   i64  G
hugepages_free    i64  G
hugepagesize      i64  G
direct_map_4k     i64  G
direct_map_2m     i64  G
direct_map_1g     i64  G
```

## `1_105_001` `/proc/loadavg`

```text
ts      ts   T
load1   f64  G
load5   f64  G
load15  f64  G
running u32  G
total   u32  G
```

## `1_106_001` `/proc/vmstat`

Схема широкая и optional: современный файл содержит около 150 ключей. Минимум,
который должен поддерживаться:

```text
ts              ts   T
pgpgin          i64  C
pgpgout         i64  C
pswpin          i64  C
pswpout         i64  C
pgfault         i64  C
pgmajfault      i64  C
pgsteal_kswapd  i64  C
pgsteal_direct  i64  C
pgscan_kswapd   i64  C
pgscan_direct   i64  C
oom_kill        i64  C
```

Остальные ключи добавляются как необязательные колонки текущей версии, если это
совместимо с правилами реестра.

## `1_107_001` `/proc/pressure/{cpu,memory,io}`

```text
ts            ts   T
resource      u8   L   // 0=cpu 1=memory 2=io
some_avg10    f32  G
some_avg60    f32  G
some_avg300   f32  G
some_total    i64  C   // usec
full_avg10    f32? G   // NULL для cpu
full_avg60    f32? G   // NULL для cpu
full_avg300   f32? G   // NULL для cpu
full_total    i64? C   // NULL для cpu
```

## `1_108_001` `/proc/diskstats`

Пишется современный формат, включая discard и flush. Поля, отсутствующие на
старых ядрах, пишутся как `NULL`.

```text
ts                   ts    T
major                u32   L
minor                u32   L
device               str   L
reads                i64   C
r_merged             i64   C
read_sectors         i64   C
read_time_ms         i64   C
writes               i64   C
w_merged             i64   C
write_sectors        i64   C
write_time_ms        i64   C
io_in_progress       i64   G
io_time_ms           i64   C
io_weighted_time_ms  i64   C
discards             i64?  C
d_merged             i64?  C
discard_sectors      i64?  C
discard_time_ms      i64?  C
flushes              i64?  C
flush_time_ms        i64?  C
```

## `1_109_001` `/proc/net/dev`

Пишутся все 16 колонок.

```text
ts             ts   T
iface          str  L
rx_bytes       i64  C
rx_packets     i64  C
rx_errs        i64  C
rx_drop        i64  C
rx_fifo        i64  C
rx_frame       i64  C
rx_compressed  i64  C
rx_multicast   i64  C
tx_bytes       i64  C
tx_packets     i64  C
tx_errs        i64  C
tx_drop        i64  C
tx_fifo        i64  C
tx_colls       i64  C
tx_carrier     i64  C
tx_compressed  i64  C
```

## `1_110_001` `/proc/net/snmp`

Широкая схема по протоколам. Минимальный набор:

```text
ts                 ts   T
tcp_active_opens   i64  C
tcp_passive_opens  i64  C
tcp_attempt_fails  i64  C
tcp_estab_resets   i64  C
tcp_in_segs        i64  C
tcp_out_segs       i64  C
tcp_retrans_segs   i64  C
tcp_in_errs        i64  C
tcp_out_rsts       i64  C
tcp_curr_estab     i64  G
udp_in_datagrams   i64  C
udp_out_datagrams  i64  C
udp_in_errors      i64  C
udp_no_ports       i64  C
```

## `1_111_001` `/proc/net/netstat`

Минимальный набор TcpExt/IpExt:

```text
ts                     ts   T
listen_overflows       i64  C
listen_drops           i64  C
tcp_timeouts           i64  C
tcp_fast_retrans       i64  C
tcp_slow_start_retrans i64  C
tcp_ofo_queue          i64  C
tcp_syn_retrans        i64  C
```

Расширение остальными ключами требует проверки совместимости. При несовместимом
изменении выпускается новая версия типа.

## `1_112_001` `mountinfo`

`on_change`, политика материализации `every_segment_last_known`. Нужен для
атрибуции `diskstats` к точкам монтирования, поэтому актуальная копия пишется в
каждый сегмент даже без изменений.

Учитываются:

- btrfs/ZFS, где subvolume может иметь `major=0`, но реальный источник — `/dev`;
- фильтрация инфраструктурных Kubernetes bind-mount вроде `/etc/hosts`, чтобы
  не приписывать I/O всего узла контейнеру.

```text
ts             ts    T
major          u32   L
minor          u32   L
mount_point    str   L
fstype         str   L
source         str   L
is_k8s_infra   bool  L
```

## `1_113_001` `cpuinfo` / topology

`on_change`, политика материализации `every_segment_last_known`.

```text
ts          ts   T
cpu_id      i32  L
model_name  str  L
mhz_max     f64  L
core_id     i32  L
socket_id   i32  L
```

## cgroup `1_200_001` - `1_204_001`

Сущность почти везде — `cgroup_path`. Первая реализация читала только свой
cgroup; новая реализация должна уметь обходить дерево `/sys/fs/cgroup`.
Глубина и фильтр обхода — настройки коллектора.

Поддержка cgroup v1 обязательна и пишет в те же `type_id`: раскладка единая,
коллектор нормализует значения на месте.

Нормализация cgroup v1:

- `cpuacct.stat` ticks -> usec через `hz`;
- `throttled_time` ns -> usec;
- `memory.stat` `rss/cache/total_*` -> `anon/file`;
- `blkio.throttle.io_service_bytes` и `io_serviced` -> `rbytes/wbytes/rios/wios`.

Лимиты считаются данными, а не метаданными: квоты могут меняться на лету,
например при Kubernetes VPA.

### `1_200_001` mapping процессов

Источник: `/proc/PID/cgroup`, период 30 секунд.

```text
ts           ts   T
pid          i32  L
starttime    ts   L
cgroup_path  str  L
```

`cgroup_path` — значение соответствия `pid -> cgroup`, а не часть ключа
сортировки. Ключ остаётся обычным entity-first: `(pid, starttime, ts)`.

Семантика `snapshot_full` выбрана осознанно, хотя это соответствие меняется
редко.
Альтернатива `on_change` экономила бы строки, но заставляла бы код чтения
искать последнее известное значение назад по сегментам перед сопоставлением с
процессными метриками. Полный снимок раз в 30 секунд дороже по объёму, зато
даёт простое сопоставление по времени внутри того же сегмента и сохраняет
самодостаточность сегмента для диагностики процессов и cgroup.

### `1_201_001` cgroup cpu

```text
ts             ts   T
cgroup_path    str  L
usage_usec     i64  C
user_usec      i64  C
system_usec    i64  C
throttled_usec i64  C
nr_throttled   i64  C
quota_usec     i64  G   // -1 = max
period_usec    i64  G
```

`throttled_usec` и `nr_throttled` важны для диагностики скрытого CPU throttling
в контейнерах.

### `1_202_001` cgroup memory

```text
ts           ts   T
cgroup_path  str  L
current      i64  G
max          i64? G   // NULL = без лимита
anon         i64  G
file         i64  G
kernel       i64  G
slab         i64  G
low_events   i64  C
high_events  i64  C
max_events   i64  C
oom_events   i64  C
oom_kill     i64  C
```

В cgroup v2 значение `max` может быть строкой `max`; в PGM это `NULL`.

### `1_203_001` cgroup io

Одна строка на устройство внутри cgroup.

```text
ts           ts   T
cgroup_path  str  L
major        u32  L
minor        u32  L
rbytes       i64  C
wbytes       i64  C
rios         i64  C
wios         i64  C
```

### `1_204_001` cgroup pids

```text
ts           ts   T
cgroup_path  str  L
current      i64  G
max          i64? G   // NULL = без лимита
```

Диапазон `1_205_001` - `1_299_999` зарезервирован под cpuset, hugetlb и будущие
контроллеры.
