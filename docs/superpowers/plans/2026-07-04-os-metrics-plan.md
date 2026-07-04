# План внедрения OS-метрик PgKronika

Дата: 2026-07-04.

База для планирования: `origin/main` после `git fetch`, commit
`942f806023b15b067872490a876026d6555a7e66`.

Источник сравнения: локальный rpglot read-only в
`/home/vadv/Projects/merge-request-reviewer/repos/gitlab.ozon.ru/infrastructure/cloudozon/rpglot`.

Этот документ является approval-артефактом. Он фиксирует границы будущей
реализации и вопросы, которые нужно утвердить до кодового PR. В этом PR не
нужно включать полноценный сбор OS-метрик.

## 0. Решения брейнсторма 2026-07-04

Раздел добавлен после совместного обсуждения. Он утверждает то, что раньше
было открытыми вопросами, и уточняет модель диска, ранее описанную
поверхностно.

### 0.1 Scope — обязательное свойство каждой строки, не режим коллектора

Главная боль предыдущего инструмента: метрики Kubernetes-ноды снимались
вместе с pod, и в интерфейсе смешивались — host-wide числа (`MemTotal` всей
ноды, host `loadavg`) показывались там, где ожидались метрики pod, давая
пугающие и ложные значения.

Решение-инвариант: **scope — свойство каждого источника, а не один флаг на
весь коллектор**. Один и тот же docker-запуск даёт разным файлам разный
scope, потому что их изолируют разные namespace:

- `/proc/stat`, `/proc/meminfo`, `/proc/loadavg`, `/proc/vmstat` — протекают
  host-wide (cgroup-лимиты в них не отражаются) → `scope=host`;
- `/proc/net/*` — сеть pod через net namespace → `scope=pod_net`;
- `/proc/PID/*` — процессы pod через pid namespace → `scope=pod`;
- cgroup (`memory.max`, `cpu.max`, throttling) — и есть pod-правда →
  `scope=container`.

Механика: **каждая OS-строка несёт колонку `scope` — 1-байтовый `u8`-enum**:
`0=host`, `1=pod`, `2=pod_net`, `3=container`, `4=unknown`. Reader
разворачивает в лейбл. Строка самодостаточна: UI не может спутать host-число
с pod-числом, потому что метка вшита в данные, а не додумывается интерфейсом.
Если детекция контейнера неуверенная — `scope=unknown`, коллектор не врёт и
не выдаёт host-число за pod.

### 0.2 Модель диска — mountinfo-фильтр (перенято из rpglot 1:1) + роль через SQL

Проверено по исходникам rpglot
(`rpglot-core/src/collector/procfs/parser/disk.rs`, `system.rs`,
`collector.rs`, `util/container.rs`): атрибуция «дисков pod» делается **чисто
через mountinfo контейнера, без обращения к PostgreSQL**. Это правильный
подход, и он перенимается целиком:

1. читать `/proc/self/mountinfo` → карта `(major:minor) → mount_paths`;
   `major=0` псевдо-устройства (overlay/proc/sysfs) пропускать;
2. реальные `major=0` (btrfs/ZFS с источником `/dev/xxx`) резолвить через
   `<sys_root>/class/block/{dev}/dev`;
3. в контейнере оставить `(major,minor)`, у которых **хотя бы один** путь НЕ
   k8s-infra bind-mount (список: `/etc/hosts`, `/etc/hostname`,
   `/etc/resolv.conf`, `/dev/termination-log`, `/run/secrets/`,
   `/var/run/secrets/`); устройства, чьи ВСЕ пути инфраструктурные,
   выбросить — они с root-диска ноды, и `/proc/diskstats` на них показал бы
   I/O всей ноды (ложные алерты);
4. `1_108 diskstats` фильтруется по этому множеству. Метка **`scope=host`**:
   это устройства, которые pod реально использует, но счётчики на них =
   нагрузка всех потребителей устройства, не только pod;
5. `1_203 cgroup io.stat` собирается **отдельной** секцией с меткой
   **`scope=container`** — точный вклад pod. Две метки рядом: «saturation
   железа» (diskstats) и «I/O pod» (cgroup) больше не смешиваются.

mountinfo-подход автоматически покрывает то, что чистый SQL-подход пропустил
бы: **WAL на отдельном volume, логи на отдельном volume, temp** — всё, что
физически смонтировано в pod, видно в mountinfo сразу, без знания
PostgreSQL-конфигурации.

**Обогащение роли (утверждено, опциональный слой поверх базы, не механизм
обнаружения):** для чисто-PG-инструмента диагностически ценно знать, какое
устройство несёт что. Поверх mountinfo-множества коллектор помечает роль
устройства через SQL: `SHOW data_directory`, реальный путь `pg_wal` (с
разыменованием симлинка), `log_directory`, `temp_tablespaces`,
`pg_tablespace_location(oid)` → каждый путь резолвится в `(major,minor)` через
ту же mountinfo-карту → устройству ставится роль (`data`/`wal`/`log`/`temp`).
Это позволяет UI различать «затык WAL-диска» и «затык data-диска». rpglot
этого не делал (он приложение-агностик); роль — дополнение, коллектор без неё
всё равно корректно атрибутирует диски. Джойн роли делает коллектор (у него
на руках оба канала — procfs и SQL); `kronika-source-os` остаётся procfs-only
и отдаёт сырой mountinfo + diskstats.

### 0.3 Решение: type_id allocation

Решение пользователя от 2026-07-04: числовая раскладка `type_id` не блокирует
approval этого design PR.

План использует `1_100/1_200` как консервативный стартовый диапазон внутри
текущих соглашений реестра PgKronika. Точные numeric IDs являются деталями
реализации. Первый implementation PR может выбрать любые свободные значения,
которые не конфликтуют с актуальным `main` и проходят registry linter.

Layout compatibility and stability start only after a concrete section is
implemented and merged. До этого `type_id` в этом документе служит
предварительной картой секций, а не approval gate.

Это решение не ослабляет контракт схемы. Строгими остаются:

- смысл каждой секции;
- field names, field types, units and nullability;
- sort keys;
- `snapshot_full`/`on_change`/event semantics;
- scope labels and source roots;
- degradation, coverage and BDD oracle contracts.

### 0.4 Открыто для следующего шага брейнсторма

Ещё не утверждено (обсудим отдельно): расхождение таймингов (этот план —
10 с база для Wave 1/2; ранее утверждённый тайминг-план — 5 с hot-path
procfs); порядок первого implementation PR (Option A vs Option B); cmdline
default; setfsuid; глубина обхода cgroup-дерева.

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

Рекомендация: не переносить OS в `2_0xx` без отдельного изменения реестра.
Использовать уже описанные `1_100/1_200` как conservative default, а термин
"волна 2" трактовать как этап внедрения, не как `type_id`.

Если первый implementation PR выберет другой свободный OS range внутри текущих
registry conventions, это допустимо. Exact numeric allocation не блокирует
approval. Блокером может быть только конфликт с реестром, schema semantics or
layout contract.

### Decision: type_id allocation

`type_id` values ниже являются предварительными. Первый implementation PR
выбирает любые currently-free IDs, сохраняет их в repo docs and registry и
проходит registry checks. Compatibility/stability starts when a section is
implemented; до реализации numeric IDs можно менять без миграции данных.

### Стартовый список секций

Таблица показывает предложенную conservative allocation, а не обязательную
финальную нумерацию.

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

- scope detector and source metadata before writing aggregate OS sections;
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

## 4. Kubernetes/pod scope contract

PgKronika поддерживает запуск как pod, поэтому OS-метрики должны иметь явную
область видимости. Нельзя записывать данные из namespace pod как метрики всей
Kubernetes node or hypervisor.

### Области видимости

Host/node scope:

- данные относятся к Kubernetes node or VM host;
- procfs/sysfs читаются из явно смонтированных host paths, например
  `/host/proc`, `/host/sys`, `/host/sys/fs/cgroup`;
- process rows относятся к host PID namespace only if deployment explicitly
  provides host PID visibility;
- все секции должны нести metadata/scope label, чтобы reader не смешивал host
  and pod views.

Container/pod scope:

- данные относятся к namespace, в котором работает collector pod;
- `/proc/stat`, `/proc/meminfo`, `/proc/loadavg`, `/proc/vmstat`,
  `/proc/pressure/*` показывают то, что ядро отдает в этом namespace;
- эти значения не являются полным node/hypervisor contract, даже если часть
  файлов выглядит host-wide;
- network sections describe current network namespace;
- diskstats in a pod can expose host devices and must be filtered or marked
  unavailable.

Process scope:

- process rows describe PIDs visible through the selected `proc_root`;
- join with `pg_stat_activity.pid` is valid only when PostgreSQL and collector
  share PID namespace or when the selected host proc root uses the same PID
  numbering as PostgreSQL reports;
- if PgKronika runs as sidecar without `shareProcessNamespace` or `hostPID`,
  PostgreSQL backend PIDs may be invisible to the collector;
- if PgKronika runs as a node agent, process rows are host processes and need a
  separate label from pod-local processes.

### Default safe behavior

Default for pod/container deployment:

- detect container/pod mode;
- record `os_scope=container` or `os_scope=pod` in instance/source metadata;
- record `proc_root`, `sys_root`, `cgroup_root`, `pid_namespace`, and detected
  container signals where feasible;
- collect only truthful namespace metrics;
- do not label namespace `/proc` values as node metrics;
- mark node metrics unavailable unless host proc/sys roots are explicitly
  configured;
- prefer cgroup metrics for CPU throttling, memory limits, OOM and PID limits
  when the collector is deployed inside Kubernetes.

Recommended first implementation behavior:

- Wave 1 may collect `/proc` core metrics in pod scope, but UI/API labels must
  say `scope=pod|container`, not `scope=node`;
- if pod deployments are the primary target, move process+cgroup foundation
  before disk/net and consider moving it before host aggregate polish;
- node scope is opt-in through explicit configuration, not inferred from
  Kubernetes environment variables;
- if scope cannot be classified, write `scope=unknown` and keep the raw source
  paths in diagnostics.

### Снимать / не снимать / снимать только при scope X

Эта таблица фиксирует решение с точки зрения Linux performance, DevOps/SRE and
DBA. Collector может сохранять raw facts, но alerting and summaries обязаны
учитывать scope.

| Сигнал | Решение | Требование scope | Обоснование |
|---|---|---|---|
| PostgreSQL backend process rows from `/proc/PID/*` | Снимать обязательно | `process` scope; корректный join требует shared PID namespace или явный host proc root с PID-нумерацией PostgreSQL | Высокая DBA-ценность: query PID -> CPU ticks, RSS, faults, state, run delay, block delay, cmdline/comm. Это главный мост между PostgreSQL and Linux. |
| Process I/O from `/proc/PID/io` | Снимать обязательно, nullable | Тот же process scope; недоступные поля пишутся как `NULL` | Высокая DBA/Linux-ценность для backend physical I/O, syscall I/O and approximation of OS page-cache behavior. Нельзя писать `0` при permission failure. |
| Cgroup CPU quota, usage and throttling | Снимать обязательно в pod/container | `target_cgroup=postgres|pod|collector|configured` должен быть явным | Для SRE это полезнее loadavg в Kubernetes: quota, usage and throttling объясняют latency under CPU limits. Collector cgroup не равен PostgreSQL cgroup без доказательства. |
| Cgroup memory and OOM events | Снимать обязательно в pod/container | То же правило `target_cgroup` | Лучше объясняет pod OOM, memory pressure and limit hits, чем `/proc/meminfo` внутри pod. |
| PSI CPU/memory/io | Снимать со scope labels | Scope выбранного `proc_root`; cgroup PSI можно добавить позже | Linux performance сигнал stall time. Полезен при известном scope; вводит в заблуждение, если pod view назвать node pressure. |
| `/proc/stat` CPU ticks | Снимать со scope labels | `scope=node` только с host proc root; иначе `scope=pod|container|unknown` | Полезно для rates and saturation trends. Само по себе не является pod capacity signal. |
| CPU count/topology | Снимать только как capacity metadata | Нужна связка с cpuset and cgroup quota/period; raw host CPU count нельзя использовать как pod capacity | CPU count искажает ratios. Effective capacity в Kubernetes задается quota/cpuset, а не обязательно online host CPUs. |
| `/proc/loadavg` | Снимать как secondary/debug; не alert в pod scope | Primary только для реального node scope; в pod scope помечать secondary | Load average смешивает runnable and uninterruptible tasks на видимом kernel scope. В container mode rpglot отключает loadavg alerting при наличии cgroup data, потому что loadavg может отражать host load, not container load. |
| `/proc/meminfo` | Снимать со scope labels | Node memory только с host proc root; pod limit брать из cgroup memory | Полезно для host cache/reclaim context. В pod deployment нельзя показывать как pod memory limit or PostgreSQL memory envelope. |
| `/proc/vmstat` | Снимать со scope labels | То же, что выбранный proc root | Полезно для faults, reclaim, swap and OOM counters, но host/pod interpretation зависит от proc scope. |
| `/proc/diskstats` | Снимать только при атрибуции | Node scope с host roots или pod/container scope после mountinfo filtering | Raw diskstats в pod могут показать node devices и дать ложную I/O attribution. Нужен filter by mount namespace and infra bind-mounts. |
| `/proc/net/dev`, SNMP, netstat | Снимать с namespace labels, ниже по DBA-приоритету | Current network namespace, если host network/proc roots не заданы явно | Полезно для SRE network errors/retransmits. Менее прямо связано с PostgreSQL без join to workload and namespace scope. |
| Host/node aggregate metrics from a normal pod | Не снимать как node metrics | Node scope unavailable | Если назвать namespace data "node metrics", получатся ложные выводы о capacity, load and pressure. |
| Node metrics from pod with host mounts | Снимать только по explicit deployment contract | read-only host `/proc`/`/sys`, configured roots and security context | Это node-agent mode. Он меняет security posture и должен быть явным. |

Решение для первого implementation PR:

- Option A: keep Wave 1 first, but include scope detector, configured roots and
  scope labels in the same PR. CPU/mem/load/PSI are stored as scoped raw facts;
  loadavg is not a primary pod alert signal; CPU count is capacity metadata only.
- Option B: if pod deployments are the main product path, move process+cgroup
  foundation earlier. This gives the DBA-critical backend join and Kubernetes
  quota/throttling/OOM signals before broad host aggregates.

Нужно утвердить: Option A для low-cardinality foundation first или Option B для
PostgreSQL backend/process+cgroup value first в Kubernetes.

### Node-level metrics from a pod

If the product requirement is node-level metrics from a pod, the deployment
must say so explicitly. A safe contract needs:

- read-only hostPath mount for `/proc`, for example
  `hostPath: /proc -> mountPath: /host/proc`;
- read-only hostPath mount for `/sys`, including `/sys/fs/cgroup` when cgroup
  node scope is needed;
- collector config such as `KRONIKA_PROC_ROOT=/host/proc`,
  `KRONIKA_SYS_ROOT=/host/sys`, `KRONIKA_CGROUP_ROOT=/host/sys/fs/cgroup`;
- `hostPID: true` or equivalent only when host process metrics and stable
  process correlation are required;
- `privileged: true` or narrowly scoped Linux capabilities only if the selected
  process files require them, for example `/proc/PID/io` for other users;
- explicit Pod Security admission exception, because hostPath, hostPID and
  privileged/capabilities are cluster security decisions;
- service account and RBAC documented separately from OS file access. Reading
  host procfs/sysfs is a node security-context issue, not an API permission.

Without this contract, PgKronika should not claim node-level CPU, memory,
process or disk metrics from a normal pod.

### Cgroup priority in pod deployments

For a pod deployment, cgroup sections may be more useful than host metrics:

- cgroup cpu shows quota, period, throttling time and throttling count;
- cgroup memory shows current usage, limits, high/max/oom/oom_kill events;
- cgroup pids shows PID pressure against pod/container limits;
- cgroup io can show container storage pressure where the controller is
  available.

The scope still needs care:

- rpglot reads the current cgroup by default in container mode; for a sidecar
  this can be the collector container, not the PostgreSQL container;
- PostgreSQL target cgroup should be discovered through PostgreSQL backend PID
  mapping when the PID namespace is shared, or through an explicit configured
  target cgroup path;
- pod-parent cgroup and individual container cgroups are different entities and
  should not share the same `cgroup_path` label.

### rpglot behavior to adopt or avoid

Observed rpglot behavior:

- container detection uses `KUBERNETES_SERVICE_HOST`, Kubernetes service
  account token path, `/.dockerenv`, `/run/.containerenv`, and `/proc/1/cgroup`
  patterns such as `kubepods`, `docker`, `containerd`, `lxc`;
- `rpglotd` and `rpglot-web` accept `--proc-path`, default `/proc`;
- cgroup collection is auto-enabled in container mode with default
  `/sys/fs/cgroup`, or explicitly via `--cgroup-path` / `--force-cgroup`;
- system collectors read all global files from the selected proc path;
- in containers, diskstats are filtered through current mountinfo and
  Kubernetes infra bind-mounts are excluded from disk attribution;
- load average alerting is skipped when a cgroup block is present, because
  `/proc/loadavg` can describe host load rather than container load;
- effective CPU count uses cgroup quota/period when available, otherwise
  SystemCpu online CPU count;
- cgroup v1 lookup uses `/proc/self/cgroup` for relative paths.

PgKronika should adopt:

- multi-signal container detection;
- explicit proc/sys/cgroup root configuration;
- automatic cgroup collection in container mode;
- diskstats filtering by mount namespace;
- Kubernetes infra mount filtering.

PgKronika should avoid:

- assuming that container-detected `/proc` means node-level metrics;
- silently mixing node scope and pod scope in one type without scope metadata;
- hardcoding `/proc/self/cgroup` when a custom `proc_root` is configured;
- hardcoding `/sys/class/block` when a custom `sys_root` is configured;
- treating cgroup metrics from the collector container as PostgreSQL pod
  metrics without target cgroup discovery.

## 5. Контракты секций

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

Контейнеры/pods: значение относится к выбранному `proc_root`. В обычном pod это
namespace/container view and must not be labeled as full node metrics. Для
container CPU-квоты нужна cgroup cpu секция.

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

Контейнеры/pods: значение относится к выбранному `proc_root`. В обычном pod это
не node contract; node scope требует явного host proc mount.

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

Контейнеры/pods: `/proc/meminfo` относится к выбранному `proc_root` and must be
scoped. Container memory limit должен читаться из cgroup memory.

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

Контейнеры/pods: load относится к выбранному `proc_root`; в pod это не заменяет
cgroup pressure and throttling.

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

Контейнеры/pods: vmstat относится к выбранному `proc_root`. Container OOM
events нужны в `1_202_001`.

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

Контейнеры/pods: зависит от видимого procfs. Cgroup PSI может потребовать
отдельного расширения, не входит в первый OS PR.

Деградация: недоступный resource логируется с `resource=cpu|memory|io`; при
частичной доступности пишутся строки только для доступных resources и отдельный
degraded log.

### `1_108_001` diskstats from `/proc/diskstats`

Источник: `/proc/diskstats`, дополнительно `/proc/self/mountinfo` и
configured `sys_root` path `<sys_root>/class/block/*/dev` для container
filtering and `major=0` resolution.

Поля: modern diskstats layout including reads/writes, sectors, times,
in-progress, weighted time, optional discard and flush fields; плюс колонка
`scope` (`u8`, см. раздел 0.1).

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

Scope: **`host`**. Даже после mountinfo-фильтрации это устройства, которые pod
реально использует, но счётчики на них — нагрузка всех потребителей
устройства, не только pod. Точный вклад pod — в `1_203_001` (cgroup io,
`scope=container`). Две метки идут рядом и не смешиваются.

Атрибуция дисков pod (перенято из rpglot 1:1, проверено по исходникам —
см. раздел 0.2):

1. `/proc/self/mountinfo` → карта `(major:minor) → mount_paths`; `major=0`
   псевдо-устройства (overlay/proc/sysfs) пропускать;
2. реальные `major=0` (btrfs/ZFS, источник `/dev/xxx`) резолвить через
   `<sys_root>/class/block/{dev}/dev`;
3. в контейнере (детекция — раздел 4) оставить `(major,minor)`, у которых
   **хотя бы один** путь НЕ k8s-infra bind-mount (`/etc/hosts`,
   `/etc/hostname`, `/etc/resolv.conf`, `/dev/termination-log`,
   `/run/secrets/`, `/var/run/secrets/`); устройства, чьи ВСЕ пути
   инфраструктурные, выбросить — иначе `/proc/diskstats` покажет I/O всей
   ноды;
4. отфильтровать diskstats по этому множеству;
5. вне контейнера — писать все устройства как есть, `scope=host` (это и есть
   правда про машину).

Роль устройства (опциональное обогащение, раздел 0.2): коллектор через SQL
(`SHOW data_directory`, разыменованный `pg_wal`, `log_directory`,
`temp_tablespaces`, `pg_tablespace_location`) резолвит пути данных в
`(major,minor)` по той же mountinfo-карте и ставит устройству роль
(`data`/`wal`/`log`/`temp`). Роль — дополнение поверх атрибуции, не механизм
обнаружения; без неё диски всё равно собираются корректно.

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

Контейнеры/pods: current network namespace. В обычном pod это pod network, not
node network.

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

Контейнеры/pods: current network namespace.

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

Контейнеры/pods: current network namespace.

Деградация: same as SNMP.

### `1_112_001` mountinfo

Источник:

- `/proc/self/mountinfo`;
- configured `sys_root` path like `/sys/class/block/*/dev` for block
  resolution;
- optionally `statvfs` in reader-side enrichment, not in current registry
  layout unless a new version is approved.

Поля: `major`, `minor`, `mount_point`, `fstype`, `source`, `is_k8s_infra`.

Единицы: labels.

Семантика: `on_change`, materialization `every_segment_last_known`.

Сортировка: `(major, minor, mount_point)`.

Кардинальность: mount points after filters, cap `KRONIKA_OS_MAX_MOUNTS`.

Nullable: current layout has no nullable fields; unresolvable entries should be
skipped only if the registry explicitly documents that.

Контейнеры/pods: current mount namespace unless host mountinfo is explicitly
configured. Kubernetes infra bind-mounts are kept with `is_k8s_infra=true` or
filtered from disk attribution according to final reader contract.

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

Контейнеры/pods: topology относится к configured proc/sys roots. В pod default
это не должно называться node inventory unless host roots are configured.

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

Контейнеры/pods:

- only PIDs visible in current PID namespace;
- host PID correlation works only if PostgreSQL and collector share the same
  namespace;
- sidecar deployment without shared process namespace may not see PostgreSQL
  backend PIDs;
- node process scope requires explicit host proc root and usually `hostPID`;
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

Контейнеры/pods: values are relative to selected proc/cgroup roots and current
cgroup namespace. Store normalized path and document whether it is host path,
pod path, collector-container path, or PostgreSQL target cgroup path.

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

Контейнеры/pods: prefer cgroup namespace path for correlation, with optional raw
path only if a later registry version adds it. Sidecar deployment must not
present collector-container memory as PostgreSQL-container memory without target
cgroup discovery.

Деградация: missing controller yields no fake row; OOM counters must never be
reset by collector-side interpretation.

### `1_203_001` cgroup io

Источники cgroup v2:

- `io.stat`.

Источники cgroup v1:

- `blkio.throttle.io_service_bytes` or `blkio.io_service_bytes`;
- `blkio.throttle.io_serviced` or `blkio.io_serviced`.

Поля: `cgroup_path`, `major`, `minor`, `rbytes`, `wbytes`, `rios`, `wios`;
плюс колонка `scope` (`u8`).

Единицы: bytes and operation counts.

Семантика: `snapshot_full`.

Сортировка: `(cgroup_path, major, minor, ts)`.

Кардинальность: cap both cgroups and `(cgroup, device)` pairs with
`KRONIKA_OS_MAX_CGROUP_IO_ROWS`.

Nullable: no fake zeros for absent controller; missing device line means absent
row.

Scope: **`container`** — это точный вклад pod в I/O по устройствам, ядро
атрибутирует его cgroup'у. Парная секция к `1_108` (diskstats, `scope=host`):
diskstats показывает saturation устройства всеми потребителями, cgroup io —
долю pod. Reader сопоставляет их по `(major,minor)`, но метки хранятся раздельно
и не смешиваются.

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

## 6. Детали rpglot по procfs and process I/O

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

## 7. Memory, OOM and performance bounds

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

## 8. BDD and test strategy

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
- Kubernetes pod detection fixture: env/service account/cgroup markers;
- normal pod fixture proving default `/proc` is `scope=pod|container`, not
  `scope=node`;
- host-mounted proc/sys fixture proving node scope is opt-in and labeled.

### FileSystem abstraction

rpglot uses a filesystem abstraction for system collectors. PgKronika should
have an equivalent, scoped to source crates:

- `ProcFsRoot` for `/proc`;
- `SysFsRoot` for `/sys`;
- `CgroupRoot` for `/sys/fs/cgroup`;
- fake filesystem for unit tests and BDD fixture roots.

This keeps CI host-independent and prevents tests from depending on current
runner `/proc`.

The abstraction must not repeat rpglot's hardcoded paths:

- cgroup relative path discovery should use configured `ProcFsRoot`, not
  literal `/proc/self/cgroup`;
- block device resolution should use configured `SysFsRoot`, not literal
  `/sys/class/block`;
- tests should cover custom roots for pod and host-mounted modes.

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
- explicit assertion that pod-scope metrics are not labeled as node-scope
  metrics.

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
- Kubernetes hostPath, hostPID or privileged permissions.

Privileged behavior like `setfsuid` retry should be tested by fake filesystem
or narrow unit tests, not by requiring host privileges.

## 9. Logging and observability

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
- `scope=node|pod|container|process|unknown`;
- `proc_root`;
- `sys_root`;
- `cgroup_root`;
- `container_detected`;
- `kubernetes_detected`;
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
- `target_cgroup=collector|postgres|pod|node|configured`.

Operator contract: absent OS section should always be explainable by either
scheduler cadence, disabled source, or degraded structured log. Silent absence
is acceptable only for documented optional sources like unsupported PSI, and
even then first occurrence should be logged.

## 10. PR decomposition

### Current PR: design only

Scope:

- add this plan under `docs/superpowers/plans/`;
- produce a user approval copy in `output_to_user`;
- no registry/code implementation;
- no behavior change.

Acceptance:

- `git diff --check` passes;
- PR opened as draft/design;
- user approves first implementation scope; exact `type_id` allocation is not
  an approval blocker.

### Implementation PR 1: Wave 1 OS core

Есть два допустимых варианта первого implementation PR:

- Option A, low-cardinality foundation: scope detector plus `1_102_001` -
  `1_107_001`.
- Option B, pod-first DBA value: scope detector plus process identity skeleton,
  backend PID join readiness and cgroup CPU/memory/PIDs before broad host
  aggregates.

Текущая рекомендация: Option A, если PgKronika сначала нужен небольшой,
low-risk OS source foundation. Выбрать Option B, если Kubernetes sidecar/pod
deployments are the primary production target.

Scope:

- OS scope detector and metadata/log labels;
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
- missing PSI yields documented degradation, not zeros;
- normal pod fixture proves metrics are scoped as pod/container;
- node scope requires explicit host proc/sys configuration;
- loadavg stored as scoped secondary data, not primary pod alert signal;
- CPU count stored only as capacity metadata with cgroup/cpuset context.

### Implementation PR 2: Wave 2 disk/net/mount

Scope:

- `1_108_001` diskstats;
- `1_109_001` net/dev;
- `1_110_001` snmp;
- `1_111_001` netstat;
- `1_112_001` mountinfo;
- `1_113_001` topology;
- mount namespace filtering for container disk attribution;
- configured `sys_root` for block device resolution.

Acceptance:

- btrfs/ZFS major=0 fixture;
- Kubernetes infra bind-mount fixture;
- old and modern diskstats fixture;
- network namespace-independent tests;
- cap behavior tested without partial full snapshot;
- pod diskstats fixture does not report unrelated node devices as pod disks.

### Implementation PR 3: Wave 3 processes

If Option B is approved, this scope moves before disk/net and can become the
first implementation PR after the scope detector.

Scope:

- `1_100_001` process hot set;
- `1_101_001` process extended status;
- limited procfs reads;
- nullable `/proc/PID/io`;
- process disappearance handling;
- cmdline caps and dictionary strategy;
- join readiness with `pg_stat_activity.pid`;
- PID namespace/scope detection for sidecar, shared process namespace and
  node-agent deployments.

Acceptance:

- process stat parser handles spaces/parentheses;
- PID reuse test uses `(pid, starttime)`;
- inaccessible `/proc/PID/io` writes NULL, not 0;
- optional cmdline failure keeps process row;
- cap/timeout does not emit silent partial full snapshot;
- no `/proc/PID/environ` collection;
- sidecar without shared PID namespace reports process scope limitation.

### Implementation PR 4: Wave 3 cgroup

Scope:

- `1_200_001` pid to cgroup mapping;
- `1_201_001` cgroup cpu;
- `1_202_001` cgroup memory;
- `1_203_001` cgroup io;
- `1_204_001` cgroup pids;
- cgroup v2 and v1 normalization;
- tree walk with root/depth/path filters;
- target cgroup selection: collector container, PostgreSQL container, pod
  parent, or explicit configured path.

Acceptance:

- v2 fixture covers cpu, memory, io, pids;
- v1 fixture covers cpuacct, memory, blkio, pids;
- unlimited values become `NULL` where registry says nullable;
- missing controller is not zero;
- group and io row caps tested;
- sidecar collector cgroup is not mislabeled as PostgreSQL cgroup.

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

## 11. Risks and approval questions

Утверждено брейнстормом 2026-07-04 (раздел 0), убрано из открытых вопросов:

- **Scope** (бывшие вопросы 6 и 9): scope — обязательная 1-байтовая `u8`-колонка
  в каждой OS-строке, вычисляется per-source; host-числа никогда не выдаются за
  pod; неуверенная детекция → `unknown`. См. 0.1.
- **Модель диска**: mountinfo-фильтр из rpglot (проверено) как база
  атрибуции + SQL-обогащение роли устройства (`data`/`wal`/`log`/`temp`) как
  опциональный слой. См. 0.2.
- **Нумерация `type_id`**: exact numeric allocation is flexible. Использовать
  любой свободный conservative range внутри текущих registry conventions;
  финальные IDs выбирает первый implementation PR. См. 0.3.

Остаются открытыми:

1. Process scope: собирать все visible PIDs или только PostgreSQL-related PIDs
   plus top-N? rpglot собирает все, но это больше данных and privacy risk.
2. Cmdline: включать `cmdline` по умолчанию, ограничивать 64 KiB and dictionary
   budget, или требовать explicit opt-in?
3. `/proc/PID/io`: начинать без `setfsuid` fallback, записывая NULL при
   отказе, или сразу включать controlled `setfsuid` retry?
4. Cgroup scope: обходить все дерево под configured root или начать с self
   cgroup plus children?
5. Node deployment contract: нужен ли официально поддержанный DaemonSet/sidecar
   mode с `hostPID`, read-only hostPath `/proc`/`/sys`, and documented security
   context, или node-level OS metrics остаются вне default deployment?
6. Cgroup target: для pod deployment считать главным collector cgroup,
   PostgreSQL target cgroup, pod-parent cgroup, or explicit configured cgroup?
7. Diagnostics: в первом кодовом PR достаточно structured logs или сразу
   вводить event/degradation section?
8. Intervals: **расхождение к разрешению** — этот план предлагает 10s база для
   Wave 1/2, но ранее утверждённый тайминг-план задаёт 5s hot-path procfs
   (cpu/meminfo/vmstat/PSI/diskstats/net). Свести к одному решению.
9. Порядок первого implementation PR: Option A, low-cardinality Wave 1 with
   strict scope metadata, или Option B, process+cgroup earlier because pod
   deployments need backend join and cgroup throttling/OOM before host
   aggregates?

Основная рекомендация: принять `1_100/1_200` как conservative default для
планирования, не блокируя approval на exact numeric IDs; Wave 1 как первый
кодовый PR, scope detector in Wave 1, pod default as truthful pod/container
metrics, node metrics only through explicit host proc/sys configuration,
cmdline enabled with cap, `/proc/PID/io` nullable without `setfsuid` in the
first process PR, loadavg secondary in pod scope, CPU count as capacity
metadata only, cgroup tree walk only after separate approval. If production
deployment is mostly Kubernetes pod/sidecar, approve Option B and move
process+cgroup earlier.

## 12. Recommended next action

Утвердить этот design PR как план, затем открыть первый implementation PR:

```text
feat(os): добавить Wave 1 procfs core metrics
```

Первый PR по Option A должен ограничиться scope detector плюс `1_102_001` -
`1_107_001`, parser fixtures, registry codecs, scheduler wiring, structured
logs and BDD fixture oracle. Это дает полезные CPU/memory/PSI данные без риска
per-PID privacy, setfsuid and cgroup cardinality problems. Loadavg остается
secondary data in pod scope.

Первый PR по Option B должен начать с process identity skeleton, safe
`/proc/PID/io` nullability model and cgroup CPU/memory/PIDs. Это быстрее даст
DBA value: backend PID -> process CPU/memory/I/O plus Kubernetes
quota/throttling/OOM context.

Перед этим нужно утвердить pod/node and implementation-order decision: default
pod mode пишет только pod/container-scoped OS data, cgroup data and explicit
scope labels; node-level metrics from a pod включаются только при явном
deployment contract with host proc/sys roots and required security context;
первым кодовым PR идет Option A or Option B.
