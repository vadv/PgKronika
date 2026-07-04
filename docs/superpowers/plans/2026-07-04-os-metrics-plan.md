# План внедрения OS-метрик PgKronika

Дата: 2026-07-04.

База для планирования: `origin/main` после `git fetch`, commit
`942f806023b15b067872490a876026d6555a7e66`.

Источник сравнения: локальный rpglot read-only в
`/home/vadv/Projects/merge-request-reviewer/repos/gitlab.ozon.ru/infrastructure/cloudozon/rpglot`.

Этот документ является approval-артефактом. Он фиксирует границы будущей
реализации и вопросы, которые нужно утвердить до кодового PR. В этом PR не
нужно включать полноценный сбор OS-метрик.

## 1. Текущее состояние PgKronika после latest main

### Что уже закрыто в SQL-поле

На текущем `main` реестр и коллектор закрывают основной PostgreSQL-пол:

- `1_001` activity;
- `1_002` pg_stat_statements;
- `1_003` store_plans ossc;
- `1_004` store_plans vadv;
- `1_005` database;
- `1_006` bgwriter/checkpointer;
- `1_007` WAL;
- `1_008` archiver;
- `1_009` pg_stat_io;
- `1_010` prepared_xacts;
- `1_011` locks and wait graph;
- `1_012` progress_vacuum;
- `1_013` user tables;
- `1_014` user indexes;
- `1_015` replication instance;
- `1_016` replication replicas;
- `1_017` replication slots;
- `1_018` wraparound risk;
- `1_019` settings;
- `1_020` reset_metadata;
- `1_021` instance_metadata;
- `1_023` collection_coverage.

Важные свойства текущей архитектуры:

- реестр является источником правды для `type_id`, семантики и раскладки;
- секции генерируются через `#[derive(Section)]`;
- коллектор сначала делает async-чтения источников, затем создает
  `SectionBuffers` и `Interner`, потому что эти структуры нельзя держать через
  await;
- для top-N источников уже есть отдельный контракт coverage;
- writer ограничивает размер секций: `MAX_SECTION_ROWS = 65_536`,
  `MAX_SECTION_BYTES = 8 MiB`, `MAX_ROW_GROUPS = 16`;
- словари имеют лимиты и проверку коллизий, поэтому нельзя переносить в
  PgKronika простой hash-only interner из rpglot.

### Что уже есть по OS в документах

`docs/type-registry/os.md` уже описывает OS и cgroup диапазоны:

- `1_100_001` - `1_113_001`: `/proc`, `/sys`, processes, CPU, memory, disk,
  network, mountinfo, topology;
- `1_200_001` - `1_204_001`: cgroup process mapping, cpu, memory, io, pids.

Фактическая реализация пока минимальная: `kronika-source-os` собирает только
host facts для `1_021_001 instance_metadata`:

- hostname;
- kernel release;
- boot_id;
- `btime`;
- `clock_ticks_per_sec`;
- `page_size_bytes`.

Остальные OS-секции пока не зарегистрированы в `kronika-registry::registry()`,
не кодируются и не пишутся коллектором.

### Какие дыры остаются

OS-домен:

- нет CPU, load, memory, vmstat, PSI;
- нет diskstats, mountinfo, net/dev, snmp, netstat;
- нет process snapshot и per-PID I/O;
- нет cgroup v1/v2 обхода и нормализации;
- нет OS BDD-фикстур и host-independent oracle.

Log-домен:

- log-секции описаны в PostgreSQL registry, но `kronika-source-log` остается
  placeholder-крейтом;
- нет tail-механики stderr/csvlog, checkpoint/autovacuum/temp/error events.

Этот план покрывает только OS-домен. Логи лучше оставить отдельным эпиком.

## 2. Нумерация OS-семьи

В устаревшей gap-карте от 2026-07-02 была предложена семья `2_0xx`. После
сверки с текущим `main` это уже конфликтует с реестром:

- `1_100_001+` закреплено за OS `/proc`, `/sys`, network, disk;
- `1_200_001+` закреплено за cgroup;
- `2_001_001+` закреплено за событиями;
- `3_001_001+` закреплено за словарями;
- `10_001_001+` закреплено за графиками.

Рекомендация: не переносить OS в `2_0xx`. Использовать уже описанные
`1_100/1_200`, а термин "волна 2" трактовать как этап внедрения, не как
`type_id`.

Если нужен именно новый OS-диапазон `2_0xx`, это отдельное решение с миграцией
реестра, потому что класс `2` уже занят event-секциями.

### Стартовый список секций

| Волна | `type_id` | Секция | Семантика | Период |
|---|---:|---|---|---|
| 1 | `1_102_001` | `/proc/stat` CPU | `snapshot_full` | базовый шаг |
| 1 | `1_103_001` | `/proc/stat` misc | `snapshot_full` | базовый шаг |
| 1 | `1_104_001` | `/proc/meminfo` | `snapshot_full` | базовый шаг |
| 1 | `1_105_001` | `/proc/loadavg` | `snapshot_full` | базовый шаг |
| 1 | `1_106_001` | `/proc/vmstat` | `snapshot_full` | базовый шаг |
| 1 | `1_107_001` | `/proc/pressure/*` | `snapshot_full` | базовый шаг |
| 2 | `1_108_001` | `/proc/diskstats` | `snapshot_full` | базовый шаг |
| 2 | `1_109_001` | `/proc/net/dev` | `snapshot_full` | базовый шаг |
| 2 | `1_110_001` | `/proc/net/snmp` | `snapshot_full` | базовый шаг |
| 2 | `1_111_001` | `/proc/net/netstat` | `snapshot_full` | базовый шаг |
| 2 | `1_112_001` | `/proc/self/mountinfo` | `on_change` | каждый сегмент |
| 2 | `1_113_001` | cpu topology | `on_change` | каждый сегмент |
| 3 | `1_100_001` | processes hot set | `snapshot_full` | 5 с |
| 3 | `1_101_001` | process status extended | `snapshot_full` | 30 с |
| 3 | `1_200_001` | pid -> cgroup mapping | `snapshot_full` | 30 с |
| 3 | `1_201_001` | cgroup cpu | `snapshot_full` | базовый шаг |
| 3 | `1_202_001` | cgroup memory | `snapshot_full` | базовый шаг |
| 3 | `1_203_001` | cgroup io | `snapshot_full` | базовый шаг |
| 3 | `1_204_001` | cgroup pids | `snapshot_full` | базовый шаг |

## 3. Волновой план

### Wave 1: CPU, memory, load, vmstat, PSI

Секции: `1_102_001` - `1_107_001`.

Почему сначала:

- низкая кардинальность;
- почти нет permission-рисков;
- нет per-PID сканирования;
- нет обхода cgroup-дерева;
- сразу появляется базовая картина: CPU saturation, memory pressure, load,
  blocked tasks, OOM/reclaim counters, PSI.

Кодовый PR после утверждения должен включать:

- procfs parser API с настраиваемым `proc_root`;
- registry codecs and contracts для `1_102` - `1_107`;
- scheduler source kind, например `OsCore`;
- structured logs per source;
- unit fixtures для procfs;
- BDD-сценарий с fixture-root и decode oracle.

### Wave 2: disk, network, mountinfo, topology

Секции: `1_108_001` - `1_113_001`.

Почему отдельно:

- diskstats и net/dev имеют среднюю кардинальность;
- diskstats в контейнере требует mount namespace фильтра;
- mountinfo должен быть `on_change` с `every_segment_last_known`;
- btrfs/ZFS и Kubernetes bind-mounts требуют отдельных фикстур;
- network SNMP/netstat широкие и зависят от версии ядра.

Ключевая адаптация из rpglot: в контейнере `/proc/diskstats` может показывать
host block devices, которые не относятся к текущему mount namespace. Нужно
сопоставлять devices с `/proc/self/mountinfo`, игнорировать инфраструктурные
Kubernetes bind-mounts (`/etc/hosts`, `/etc/hostname`, `/etc/resolv.conf`,
secrets, termination-log) и не раздувать сегменты host-wide списком устройств.

### Wave 3: processes and cgroup

Секции: `1_100_001`, `1_101_001`, `1_200_001` - `1_204_001`.

Почему последней:

- самая высокая кардинальность;
- per-PID файлы могут исчезать во время сканирования;
- `/proc/PID/io` имеет сложную permission-модель;
- cmdline чувствителен с точки зрения приватности;
- cgroup v1/v2 требуют нормализации в единый layout;
- tree walk `/sys/fs/cgroup` нуждается в лимитах глубины, количества групп и
  `(cgroup, device)` строк.

Эта волна дает главную диагностическую ценность rpglot: связь
`pg_stat_activity.pid` -> process -> CPU, memory, I/O, cgroup throttling/OOM.

## 4. Контракты секций

Общее правило: если секция заявлена как `snapshot_full`, коллектор не должен
молча писать частичный снимок как полный. При срабатывании лимита или тайм-аута
нужно одно из двух:

- не писать секцию и записать structured log плюс диагностический сигнал;
- писать секцию с явно утвержденной partial/top-N семантикой и coverage.

Для OS стартовая рекомендация: не менять семантику на partial без отдельного
type/version bump.

### `1_102_001` CPU from `/proc/stat`

Источник: `/proc/stat`, строки `cpu` and `cpuN`.

Поля:

- `ts`;
- `cpu_id`, `NULL` для агрегатной строки `cpu`;
- `user`, `nice`, `system`, `idle`, `iowait`, `irq`, `softirq`, `steal`,
  `guest`, `guest_nice`.

Единицы: scheduler ticks. Конвертация в секунды делается reader-стороной через
`clock_ticks_per_sec` из `1_021_001`.

Семантика: `snapshot_full`.

Сортировка: `(cpu_id, ts)`, агрегатная строка до per-cpu строк.

Кардинальность: `1 + online_cpu_count`, с защитным лимитом
`KRONIKA_OS_MAX_CPUS`.

Nullable: только `cpu_id` для агрегата. Отсутствующие trailing поля старых ядер
должны трактоваться как `0` только если это соответствует `/proc/stat`
контракту ядра; иначе parse error.

Права: обычное чтение `/proc/stat`.

Контейнеры: значение обычно host-visible, не cgroup-limited. Для контейнерной
CPU-квоты нужна cgroup cpu секция.

Деградация: при read/parse error секция не пишется, лог:
`action=collection_degraded collection=os_cpu source=/proc/stat reason=...`.

### `1_103_001` misc from `/proc/stat`

Источник: `/proc/stat`, строки `ctxt`, `processes`, `procs_running`,
`procs_blocked`, `btime`.

Поля:

- `ts`;
- `ctxt`;
- `processes`;
- `procs_running`;
- `procs_blocked`;
- `btime`.

Единицы:

- counters в единицах ядра;
- `btime` как unix microseconds, как в `1_021_001`.

Семантика: `snapshot_full`.

Сортировка: `(ts)`.

Кардинальность: 1 строка.

Nullable: нет. Если `btime` отсутствует или не парсится, секция не пишется.

Контейнеры: host-visible по текущему `/proc`.

Деградация: отдельная от CPU, чтобы CPU строки могли быть записаны даже если
misc-строка повреждена.

### `1_104_001` memory from `/proc/meminfo`

Источник: `/proc/meminfo`.

Поля: широкий набор из `docs/type-registry/os.md`: total/free/available,
buffers, cached, swap, active/inactive, dirty/writeback, anon, mapped, shmem,
slab, page tables, commit, huge pages, direct map.

Единицы: KiB для memory fields; `HugePages_*` в штуках; `Hugepagesize` в KiB.

Семантика: `snapshot_full`.

Сортировка: `(ts)`.

Кардинальность: 1 строка.

Nullable: ключи, отсутствующие на конкретном ядре, пишутся `NULL`, не `0`.

Права: обычное чтение.

Контейнеры: обычно host memory, не container limit. Container memory limit
должен читаться из cgroup memory.

Деградация: если файл отсутствует или обязательный `MemTotal` не парсится,
секция не пишется; optional keys остаются `NULL`.

### `1_105_001` load from `/proc/loadavg`

Источник: `/proc/loadavg`.

Поля: `load1`, `load5`, `load15`, `running`, `total`.

Единицы: load average and thread counts.

Семантика: `snapshot_full`.

Сортировка: `(ts)`.

Кардинальность: 1 строка.

Nullable: нет.

Контейнеры: host-visible; не заменяет cgroup pressure.

Деградация: секция не пишется при parse error.

### `1_106_001` vmstat from `/proc/vmstat`

Источник: `/proc/vmstat`.

Минимальные поля:

- `pgpgin`, `pgpgout`;
- `pswpin`, `pswpout`;
- `pgfault`, `pgmajfault`;
- `pgsteal_kswapd`, `pgsteal_direct`;
- `pgscan_kswapd`, `pgscan_direct`;
- `oom_kill`.

Единицы: kernel counters.

Семантика: `snapshot_full`.

Сортировка: `(ts)`.

Кардинальность: 1 строка.

Nullable: отсутствующие optional keys пишутся `NULL`. Минимальные поля лучше
тоже сделать nullable, если реестр разрешит, чтобы старые ядра не ломали всю
секцию. Если текущий контракт оставляет их required, read error должен быть
явной деградацией.

Контейнеры: host-visible; container OOM events нужны в `1_202_001`.

Деградация: отсутствующий файл или невозможный parse дает degraded log.

### `1_107_001` PSI from `/proc/pressure/{cpu,memory,io}`

Источники:

- `/proc/pressure/cpu`;
- `/proc/pressure/memory`;
- `/proc/pressure/io`.

Поля:

- `resource`: `0=cpu`, `1=memory`, `2=io`;
- `some_avg10`, `some_avg60`, `some_avg300`, `some_total`;
- `full_avg10`, `full_avg60`, `full_avg300`, `full_total`.

Единицы:

- averages in percent-like PSI values from kernel;
- totals in microseconds.

Семантика: `snapshot_full` для доступных resources.

Сортировка: `(resource, ts)`.

Кардинальность: 0-3 строки.

Nullable:

- `full_*` для CPU пишутся `NULL`;
- если whole PSI subsystem отсутствует, секция отсутствует и логируется как
  unavailable, не как нули.

Права: обычное чтение.

Контейнеры: зависит от видимого procfs. Cgroup PSI может потребовать отдельного
расширения, не входит в первый OS PR.

Деградация: недоступный resource логируется с `resource=cpu|memory|io`; при
частичной доступности пишутся строки только для доступных resources и отдельный
degraded log.

### `1_108_001` diskstats from `/proc/diskstats`

Источник: `/proc/diskstats`, дополнительно `/proc/self/mountinfo` и
`/sys/class/block/*/dev` для container filtering and major=0 resolution.

Поля: modern diskstats layout including reads/writes, sectors, times,
in-progress, weighted time, optional discard and flush fields.

Единицы:

- sectors как raw sector count, reader переводит в bytes через 512-byte sector
  для стандартных Linux diskstats rates;
- times in milliseconds;
- counters are cumulative.

Семантика: `snapshot_full`.

Сортировка: `(major, minor, ts)`.

Кардинальность: block devices visible after filtering, cap
`KRONIKA_OS_MAX_BLOCK_DEVS`.

Nullable: discard/flush columns `NULL` on older kernels.

Права: обычное чтение.

Контейнеры:

- в контейнере нельзя blindly писать все host devices;
- применить rpglot-подход: соотнести devices с mountinfo текущего namespace;
- отфильтровать Kubernetes infra bind-mounts;
- для btrfs/ZFS major=0 пробовать resolve через `/sys/class/block`.

Деградация: если cap сработал, не выдавать partial full. Лучше пропустить
секцию и логировать `reason=cardinality_cap`, затем добавить coverage/event в
следующем PR.

### `1_109_001` net/dev from `/proc/net/dev`

Источник: `/proc/net/dev`.

Поля: все 16 RX/TX колонок из registry.

Единицы: bytes, packets, errors, drops and other cumulative counters.

Семантика: `snapshot_full`.

Сортировка: `(iface, ts)`.

Кардинальность: interfaces count, cap `KRONIKA_OS_MAX_NET_IFACES`.

Nullable: нет для строк, которые успешно распарсились.

Права: обычное чтение.

Контейнеры: network namespace current process.

Деградация: malformed interface line не должна ломать весь collector process;
секция не пишется либо пишется только после утверждения partial semantics.

### `1_110_001` SNMP from `/proc/net/snmp`

Источник: `/proc/net/snmp`.

Минимальные поля: TCP active/passive opens, fails, resets, in/out segments,
retrans, errors, current established, UDP datagrams/errors/no ports.

Единицы: cumulative counters, кроме current established as gauge.

Семантика: `snapshot_full`.

Сортировка: `(ts)`.

Кардинальность: 1 строка.

Nullable: optional protocol keys `NULL` if absent.

Контейнеры: current network namespace.

Деградация: if protocol block absent, write nullable fields when possible;
otherwise skip section with structured log.

### `1_111_001` netstat from `/proc/net/netstat`

Источник: `/proc/net/netstat`.

Минимальные поля: `ListenOverflows`, `ListenDrops`, timeouts,
fast/slow retransmits, OFO queue, SYN retransmits.

Единицы: cumulative counters.

Семантика: `snapshot_full`.

Сортировка: `(ts)`.

Кардинальность: 1 строка.

Nullable: optional keys `NULL`.

Контейнеры: current network namespace.

Деградация: same as SNMP.

### `1_112_001` mountinfo

Источник:

- `/proc/self/mountinfo`;
- `/sys/class/block/*/dev` for block resolution;
- optionally `statvfs` in reader-side enrichment, not in current registry
  layout unless a new version is approved.

Поля: `major`, `minor`, `mount_point`, `fstype`, `source`, `is_k8s_infra`.

Единицы: labels.

Семантика: `on_change`, materialization `every_segment_last_known`.

Сортировка: `(major, minor, mount_point)`.

Кардинальность: mount points after filters, cap `KRONIKA_OS_MAX_MOUNTS`.

Nullable: current layout has no nullable fields; unresolvable entries should be
skipped only if the registry explicitly documents that.

Контейнеры: current mount namespace. Kubernetes infra bind-mounts are kept with
`is_k8s_infra=true` or filtered from disk attribution according to final
reader contract.

Деградация: if mountinfo unavailable, disk attribution must degrade
independently from diskstats collection.

### `1_113_001` cpu topology

Источник:

- `/proc/cpuinfo`;
- `/sys/devices/system/cpu/cpu*/topology/*`;
- optionally cpufreq max frequency files when present.

Поля: `cpu_id`, `model_name`, `mhz_max`, `core_id`, `socket_id`.

Единицы: labels and MHz.

Семантика: `on_change`, materialization `every_segment_last_known`.

Сортировка: `(cpu_id)`.

Кардинальность: CPU count, cap `KRONIKA_OS_MAX_CPUS`.

Nullable: if topology files are absent, prefer `NULL`/sentinel only after
registry version confirms layout. Current required fields mean degraded skip is
safer than fabricated values.

Контейнеры: host-visible CPU topology; cgroup quota remains separate.

### `1_100_001` processes hot set

Источники per PID:

- `/proc/PID/stat`;
- `/proc/PID/status`;
- `/proc/PID/io`;
- `/proc/PID/schedstat`;
- `/proc/PID/comm`;
- `/proc/PID/cmdline`.

Поля: identity, uid/gid, state, scheduler ticks, run delay, block delay,
context switches, faults, memory gauges, optional I/O counters.

Ключ сущности: `(pid, starttime)`, где `starttime` должен быть unix
microseconds:

```text
starttime_usec = btime_usec + starttime_ticks * 1_000_000 / clock_ticks_per_sec
```

Единицы:

- `utime`, `stime`, `blkdelay_ticks` in ticks;
- `rundelay_ns` in nanoseconds;
- memory in KiB;
- `/proc/PID/io` counters in bytes or syscall counts according to kernel
  field name.

Семантика: `snapshot_full`.

Сортировка: `(pid, starttime, ts)`.

Кардинальность: all visible PIDs, bounded by `KRONIKA_OS_MAX_PIDS`.

Nullable:

- `/proc/PID/io` fields are nullable and must not be synthesized as `0`;
- `cmdline` is nullable;
- optional scheduler/status values may be nullable only if registry layout is
  versioned accordingly.

Права:

- `stat`, `status`, `comm` are usually readable;
- `/proc/PID/io` can fail for another uid, `hidepid`, Yama,
  SELinux/AppArmor, container PID namespace;
- `/proc/PID/cmdline` can fail or be empty.

Контейнеры:

- only PIDs visible in current PID namespace;
- host PID correlation works only if PostgreSQL and collector share the same
  namespace;
- cgroup mapping is separate in `1_200_001`.

Деградация:

- if `stat` or required identity parse fails because process disappeared, skip
  row and increment `process_gone`;
- if optional files disappear after `stat`, keep row with nullable optional
  fields;
- if PID cap or scan budget is exceeded, do not emit partial
  `snapshot_full`; log degraded and add coverage/event in the same code PR.

### `1_101_001` process status extended

Источник: `/proc/PID/status` and identity from `/proc/PID/stat`.

Поля: `vm_data`, `vm_stk`, `vm_lib`, `vm_lck`, `vm_pte`, `vm_peak`,
`vm_hwm`, `threads`, `fdsize`, voluntary and nonvoluntary context switches.

Единицы: KiB for memory, counters for context switches.

Семантика: `snapshot_full`, slower cadence than `1_100_001`.

Сортировка: `(pid, starttime, ts)`.

Кардинальность: same PID cap as processes.

Nullable: absent optional status keys should become `NULL` in a new version;
current required layout needs explicit parse requirements.

Деградация: same process disappearance rules as `1_100_001`.

### `1_200_001` process to cgroup mapping

Источники:

- `/proc/PID/cgroup`;
- `/proc/PID/stat` for `(pid, starttime)`.

Поля: `pid`, `starttime`, `cgroup_path`.

Единицы: labels.

Семантика: `snapshot_full` every 30 seconds.

Сортировка: `(pid, starttime, ts)`.

Кардинальность: visible PIDs.

Nullable: no nullable fields in current layout.

Контейнеры: values are relative to current cgroup namespace. Store normalized
path and document whether it is host path or namespace path.

Деградация: process gone means skip mapping row; PID cap means no partial full.

### `1_201_001` cgroup cpu

Источники cgroup v2:

- `cpu.max`;
- `cpu.stat`.

Источники cgroup v1:

- `cpuacct.usage`;
- `cpuacct.stat`;
- `cpu.cfs_quota_us`;
- `cpu.cfs_period_us`;
- `cpu.stat`.

Поля: usage/user/system/throttled usec, nr_throttled, quota, period.

Единицы:

- usec for CPU time and throttling time;
- v1 `cpuacct.stat` ticks converted through `clock_ticks_per_sec`;
- v1 throttled_time ns converted to usec;
- `quota_usec = -1` means unlimited in current registry.

Семантика: `snapshot_full`.

Сортировка: `(cgroup_path, ts)`.

Кардинальность: cgroups after root/depth/filter, cap `KRONIKA_OS_MAX_CGROUPS`.

Nullable: missing controller should omit cgroup cpu row or use nullable fields
only after layout update. Do not write fake zeros.

Деградация: controller absent vs read denied must be distinguishable in logs.

### `1_202_001` cgroup memory

Источники cgroup v2:

- `memory.current`;
- `memory.max`;
- `memory.stat`;
- `memory.events`.

Источники cgroup v1:

- `memory.usage_in_bytes`;
- `memory.limit_in_bytes`;
- `memory.stat`;
- `memory.failcnt`.

Поля: current, max, anon, file, kernel, slab, low/high/max/oom/oom_kill events.

Единицы: bytes and cumulative event counters.

Семантика: `snapshot_full`.

Сортировка: `(cgroup_path, ts)`.

Nullable: `max = NULL` for unlimited (`max` in v2 or huge v1 limit).

Контейнеры: prefer cgroup namespace path for correlation, with optional raw path
only if a later registry version adds it.

Деградация: missing controller yields no fake row; OOM counters must never be
reset by collector-side interpretation.

### `1_203_001` cgroup io

Источники cgroup v2:

- `io.stat`.

Источники cgroup v1:

- `blkio.throttle.io_service_bytes` or `blkio.io_service_bytes`;
- `blkio.throttle.io_serviced` or `blkio.io_serviced`.

Поля: `cgroup_path`, `major`, `minor`, `rbytes`, `wbytes`, `rios`, `wios`.

Единицы: bytes and operation counts.

Семантика: `snapshot_full`.

Сортировка: `(cgroup_path, major, minor, ts)`.

Кардинальность: cap both cgroups and `(cgroup, device)` pairs with
`KRONIKA_OS_MAX_CGROUP_IO_ROWS`.

Nullable: no fake zeros for absent controller; missing device line means absent
row.

Деградация: cap or timeout means skip section and log coverage gap.

### `1_204_001` cgroup pids

Источники:

- `pids.current`;
- `pids.max`.

Поля: current and max.

Единицы: process count.

Семантика: `snapshot_full`.

Сортировка: `(cgroup_path, ts)`.

Nullable: `max = NULL` for unlimited.

Деградация: absent controller is not zero.

## 5. Детали rpglot по procfs and process I/O

rpglot важен не столько списком полей, сколько накопленными правилами для
Linux procfs.

### Что читает rpglot

Per-process модель rpglot читает:

- `/proc/PID/stat` for pid, ppid, state, tty, faults, CPU ticks, priority,
  threads, start time, vsize, rss, current CPU, RT priority, scheduler policy,
  `delayacct_blkio_ticks`;
- `/proc/PID/status` for uid/gid, memory gauges, voluntary and nonvoluntary
  context switches;
- `/proc/PID/io` for `rchar`, `wchar`, `syscr`, `syscw`, `read_bytes`,
  `write_bytes`, `cancelled_write_bytes`;
- `/proc/PID/schedstat` for run time and run delay;
- `/proc/PID/comm` and `/proc/PID/cmdline`.

`/proc/PID/stat` парсится через первую `(` и последнюю `)`, потому что `comm`
может содержать пробелы and parentheses.

### `/proc/PID/io`: доступ и nullable semantics

rpglot пытается читать `/proc/PID/io` напрямую. Если получает
`PermissionDenied` для чужого uid, он пробует временно переключить filesystem
uid/gid через `setfsuid`/`setfsgid`, затем повторяет read. Если повторный read
не удался, rpglot warning пишет один раз и возвращает default-zero `ProcIo`.

Для PgKronika нужно адаптировать это безопаснее:

- не писать нули при невозможности чтения `/proc/PID/io`;
- все I/O поля процесса должны быть `NULL` для inaccessible PID;
- логировать счетчики `io_permission_denied`, `io_read_failed`,
  `io_setfsuid_retry`;
- если будет выбран `setfsuid` fallback, выполнять его только в изолированном
  blocking/thread-local контексте, не в мигрирующей async task;
- всегда восстанавливать fsuid/fsgid, даже при ошибке;
- иметь конфиг для отключения credential switching, например
  `KRONIKA_PROC_IO_SETFSUID=off`.

Причина осторожности: `setfsuid` меняет credentials потока. В async runtime
нельзя держать такой state через await или переносить работу между worker
threads. Для PgKronika проще и безопаснее начать с режима "без setfsuid:
недоступные I/O поля = NULL", а fallback добавить отдельным утвержденным PR.

### Семантика полей `/proc/PID/io`

- `rchar` and `wchar`: bytes passed through read/write-like syscalls, включая
  page cache;
- `syscr` and `syscw`: count of read/write-like syscalls;
- `read_bytes` and `write_bytes`: bytes that caused storage I/O;
- `cancelled_write_bytes`: bytes whose writeback was cancelled, for example
  because file was truncated before flush.

Для диагностики PostgreSQL важны обе пары:

- `rchar - read_bytes` дает приближение OS page cache hit для backend read path;
- `read_bytes/write_bytes` показывают physical storage pressure.

### Унаследованный I/O умерших детей

rpglot содержит отдельную коррекцию: когда child process завершается, Linux
может добавить его cumulative `/proc/PID/io` counters к parent через `wait()`.
Это создает phantom spikes у supervisor-процессов, например postmaster or
systemd. rpglot:

- сравнивает current and previous process snapshots;
- находит процессы, которые были в previous и исчезли в current;
- группирует их cumulative I/O по `ppid`;
- subtracts known inherited counters from parent delta;
- исключает parent PIDs from process I/O hog detection, because residual I/O
  between previous snapshot and child death is unknowable.

Для PgKronika:

- collector должен писать raw cumulative counters;
- correction должна жить на reader/analysis side;
- ключ сравнения должен быть `(pid, starttime)`, а не только `pid`, иначе PID
  reuse даст ложные deltas;
- process I/O anomaly rules должны понижать confidence или исключать parent
  processes with children;
- acceptance tests должны иметь сценарий child death with inherited I/O.

### Process disappearance

rpglot пропускает процесс, если обязательные файлы исчезли между `read_dir`
and parse. Для PgKronika это правильная база:

- исчезновение процесса во время скана не является error;
- optional-файл, который исчез после успешного identity parse, дает NULL;
- метрики `pids_seen`, `pids_collected`, `pids_gone`, `optional_read_failed`
  должны попадать в structured logs.

## 6. Memory, OOM and performance bounds

### Ограничения памяти

Запрещены unbounded reads from procfs/sysfs. Нужен общий helper
`read_proc_file_limited`:

- small files (`stat`, `status`, `io`, `schedstat`) read with tight byte caps;
- `cmdline` read with separate cap, например 64 KiB before dictionary insert;
- mountinfo, diskstats, net files read with configured max bytes;
- parse should stream/split lines without retaining duplicate large strings.

Словари:

- использовать существующий PgKronika interner/dictionary, not rpglot
  hash-only interner;
- keep collision-safe behavior;
- set OS dictionary budget explicitly, for example same order as current
  activity dictionary: 4096 strings, 64 KiB blob max, 16 MiB total;
- truncate or drop `cmdline` before dictionary pressure can break segment
  limits;
- never collect `/proc/PID/environ`, registry explicitly forbids it.

Section caps:

- respect `MAX_SECTION_ROWS = 65_536`;
- respect `MAX_SECTION_BYTES = 8 MiB`;
- validate config caps at startup so `max_pids`, `max_cgroups`,
  `max_cgroup_io_rows` cannot exceed writer constraints.

### Scan cost and deadlines

Recommended first defaults:

- `KRONIKA_OS_BASE_INTERVAL_SECS=10` for Wave 1 and Wave 2;
- processes hot set every 5 seconds only after Wave 3 approval;
- process extended status and cgroup mapping every 30 seconds;
- cgroup cpu/memory/io/pids every base step or 10 seconds;
- `KRONIKA_OS_SCAN_BUDGET_MS` with per-source budget logs.

If a source exceeds budget:

- do not emit fake empty snapshot;
- do not emit partial full snapshot silently;
- log `reason=timeout`;
- add coverage/degradation event when event section is available.

### OOM safety

OS collector must be conservative under memory pressure:

- avoid building two full copies of process cmdlines;
- avoid storing all cgroup file contents before parsing;
- keep top-level vectors pre-sized by observed count but cap before allocation;
- use `try_reserve` where a hostile procfs tree could inflate counts;
- never panic on integer overflow, use checked/saturating conversions with
  degraded logs.

## 7. BDD and test strategy

### Unit parser fixtures

Add fixture-driven parser tests in `kronika-source-os`:

- `/proc/stat` with aggregate and per-cpu rows;
- `/proc/meminfo` from multiple kernels with missing optional keys;
- `/proc/loadavg`;
- `/proc/vmstat`;
- PSI present and PSI absent;
- `diskstats` old layout and modern discard/flush layout;
- `net/dev`, `snmp`, `netstat`;
- process `stat` with spaces and parentheses in `comm`;
- process disappears during optional reads;
- `/proc/PID/io` permission denied;
- cgroup v2 and v1 trees.

### FileSystem abstraction

rpglot uses a filesystem abstraction for system collectors. PgKronika should
have an equivalent, scoped to source crates:

- `ProcFsRoot` for `/proc`;
- `SysFsRoot` for `/sys`;
- `CgroupRoot` for `/sys/fs/cgroup`;
- fake filesystem for unit tests and BDD fixture roots.

This keeps CI host-independent and prevents tests from depending on current
runner `/proc`.

### BDD oracle mechanics

Extend the current BDD harness with fixture-root steps already anticipated in
`docs/testing.md`:

```text
переписать файл фикстурного дерева /proc: <path>
переписать файл фикстурного дерева cgroup: <path>
ждать появления секции <type_id>
прочитать секцию <type_id> и сравнить строки с oracle
```

Oracle rules:

- exact rows for fixture-based tests;
- transformed values for ticks to usec only when reader does conversion;
- subset/floor/ceiling only for explicitly live-host scenarios;
- golden layout tests for each new registry section;
- no CI assertion on real host CPU count, interface count, or cgroup shape.

### Golden layouts

For every new type:

- registry linter must pass;
- empty section must decode through `decode_any`;
- one fixture section must roundtrip encode/decode;
- sort key must be asserted;
- nullable columns must be tested with at least one NULL row.

### CI considerations

The CI must not require:

- root;
- `CAP_SYS_PTRACE`;
- writable cgroup;
- specific kernel PSI support;
- specific network interface names.

Privileged behavior like `setfsuid` retry should be tested by fake filesystem
or narrow unit tests, not by requiring host privileges.

## 8. Logging and observability

Use the structured stderr logging convention from PR #48/current collector:
one logfmt line with prefix `pg_kronika-collector`, `level`, `action`, then
stable fields.

Required OS actions:

- `collection_start`;
- `collection_finish`;
- `collection_degraded`;
- `collection_skipped`;
- `collection_cap_hit`;
- `collection_parse_error`.

Common fields:

- `collection=os_cpu|os_mem|os_load|os_vmstat|os_psi|os_disk|os_net|os_process|cgroup`;
- `type_id`;
- `layout_id`;
- `source=/proc/stat` or exact source group;
- `rows`;
- `elapsed_ms`;
- `bytes_read`;
- `budget_ms`;
- `reason`;
- `forced`;
- `segment_id` if available.

Process-specific fields:

- `pids_seen`;
- `pids_collected`;
- `pids_gone`;
- `cmdline_null`;
- `io_null`;
- `io_permission_denied`;
- `io_setfsuid_retry`;
- `pid_cap`;
- `scan_budget_ms`.

Cgroup-specific fields:

- `cgroups_seen`;
- `cgroups_collected`;
- `controllers_missing`;
- `cgroup_io_rows`;
- `cgroup_depth_cap`;
- `cgroup_filter`.

Operator contract: absent OS section should always be explainable by either
scheduler cadence, disabled source, or degraded structured log. Silent absence
is acceptable only for documented optional sources like unsupported PSI, and
even then first occurrence should be logged.

## 9. PR decomposition

### Current PR: design only

Scope:

- add this plan under `docs/superpowers/plans/`;
- produce a user approval copy in `output_to_user`;
- no registry/code implementation;
- no behavior change.

Acceptance:

- `git diff --check` passes;
- PR opened as draft/design;
- user approves numbering and first implementation scope.

### Implementation PR 1: Wave 1 OS core

Scope:

- `kronika-source-os` procfs parser foundation;
- `1_102_001` CPU;
- `1_103_001` stat misc;
- `1_104_001` meminfo;
- `1_105_001` loadavg;
- `1_106_001` vmstat;
- `1_107_001` PSI;
- scheduler source kind and env config;
- structured logs;
- unit fixtures and one BDD fixture scenario.

Acceptance:

- registry linter passes;
- `cargo fmt --all -- --check`;
- `cargo clippy --workspace --all-targets -- -D warnings`;
- `cargo test --workspace`;
- `cargo run -p xtask -- check-deps`;
- no unbounded procfs reads;
- no panic path on malformed fixture;
- missing PSI yields documented degradation, not zeros.

### Implementation PR 2: Wave 2 disk/net/mount

Scope:

- `1_108_001` diskstats;
- `1_109_001` net/dev;
- `1_110_001` snmp;
- `1_111_001` netstat;
- `1_112_001` mountinfo;
- `1_113_001` topology;
- mount namespace filtering for container disk attribution.

Acceptance:

- btrfs/ZFS major=0 fixture;
- Kubernetes infra bind-mount fixture;
- old and modern diskstats fixture;
- network namespace-independent tests;
- cap behavior tested without partial full snapshot.

### Implementation PR 3: Wave 3 processes

Scope:

- `1_100_001` process hot set;
- `1_101_001` process extended status;
- limited procfs reads;
- nullable `/proc/PID/io`;
- process disappearance handling;
- cmdline caps and dictionary strategy;
- join readiness with `pg_stat_activity.pid`.

Acceptance:

- process stat parser handles spaces/parentheses;
- PID reuse test uses `(pid, starttime)`;
- inaccessible `/proc/PID/io` writes NULL, not 0;
- optional cmdline failure keeps process row;
- cap/timeout does not emit silent partial full snapshot;
- no `/proc/PID/environ` collection.

### Implementation PR 4: Wave 3 cgroup

Scope:

- `1_200_001` pid to cgroup mapping;
- `1_201_001` cgroup cpu;
- `1_202_001` cgroup memory;
- `1_203_001` cgroup io;
- `1_204_001` cgroup pids;
- cgroup v2 and v1 normalization;
- tree walk with root/depth/path filters.

Acceptance:

- v2 fixture covers cpu, memory, io, pids;
- v1 fixture covers cpuacct, memory, blkio, pids;
- unlimited values become `NULL` where registry says nullable;
- missing controller is not zero;
- group and io row caps tested.

### Follow-up PR 5: reader/analysis rates

Scope:

- CPU/disk/net/process rates;
- process I/O child-death correction;
- backend OS cache hit approximation from `rchar` and `read_bytes`;
- UI/API surfacing if applicable;
- charts under `10_001_001+` only after raw sections are stable.

Acceptance:

- deltas keyed by stable entity identity, not PID alone;
- reset and wraparound handling;
- tests for process death inherited I/O;
- parent process I/O confidence documented.

## 10. Risks and approval questions

Approval needed:

1. Нумерация: подтвердить, что OS остается в `1_100/1_200`, а не переносится в
   `2_0xx`.
2. Process scope: собирать все visible PIDs или только PostgreSQL-related PIDs
   plus top-N? rpglot собирает все, но это больше данных and privacy risk.
3. Cmdline: включать `cmdline` по умолчанию, ограничивать 64 KiB and dictionary
   budget, или требовать explicit opt-in?
4. `/proc/PID/io`: начинать без `setfsuid` fallback, записывая NULL при
   отказе, или сразу включать controlled `setfsuid` retry?
5. Cgroup scope: обходить все дерево под configured root или начать с self
   cgroup plus children?
6. Container semantics: считать `/proc/stat`, `/proc/meminfo`, `/proc/loadavg`
   host-visible метриками и явно отделить их от cgroup limits?
7. Diagnostics: в первом кодовом PR достаточно structured logs или сразу
   вводить event/degradation section?
8. Intervals: принять стартовые 10s для Wave 1/2, 5s process hot set, 30s
   process extended and mapping?

Основная рекомендация: утвердить `1_100/1_200`, Wave 1 как первый кодовый PR,
cmdline enabled with cap, `/proc/PID/io` nullable without `setfsuid` in the
first process PR, cgroup tree walk only after separate approval.

## 11. Recommended next action

Утвердить этот design PR как план, затем открыть первый implementation PR:

```text
feat(os): добавить Wave 1 procfs core metrics
```

Первый PR должен ограничиться `1_102_001` - `1_107_001`, parser fixtures,
registry codecs, scheduler wiring, structured logs and BDD fixture oracle. Это
дает полезные CPU/memory/load/PSI данные без риска per-PID privacy, setfsuid and
cgroup cardinality problems.
