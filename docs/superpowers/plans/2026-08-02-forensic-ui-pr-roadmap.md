# PgKronika: план реализации forensic UI

Дата: 2 августа 2026 года  
Статус: принят к реализации  
Аудитория: maintainers PgKronika, PostgreSQL DBA, DevOps- и Linux performance-инженеры  
Базовый экран: 1920×1080

## Результат

Новый интерфейс реализуется как серия из восьми проверяемых PR. Каждый PR оставляет репозиторий в рабочем состоянии, сохраняет строгие ограничения памяти и не выдаёт временное совпадение за доказанную причинную связь.

Ветки образуют стек: первый PR направляется в `main`, каждый следующий — в ветку предыдущего PR. После слияния нижнего PR следующий PR перебазируется или перенаправляется на `main`. Это позволяет вести реализацию последовательно, не смешивая ревью независимых слоёв.

## Термины

| Понятие | Термин | Точное имя в API |
| --- | --- | --- |
| Точная связь | exact | `exact` |
| Связь по lifetime процесса | lifetime | `lifetime` |
| Совпадение в одном временном окне | temporal | `temporal` |
| Приближённое соответствие | best effort | `best_effort` |
| Связь недоступна | unavailable | `unavailable` |
| Сводная временная полоса | Health line | `HealthLine` |
| Подготовленный аналитический срез | разрез | `lens` |
| Происхождение и ограничения значения | provenance | `ProvenancePopover` |

## Последовательность PR

### PR 1. Честная семантика связей

Цель: ввести закрытый словарь качества связей и убрать ложную точность из Activity и Statement→Plan.

Изменения:

- добавить `RelationKind` в UI catalog и Entity API;
- заменить сравнение `backend_start=starttime` на явно приближённое сопоставление по PID в одном снимке;
- продолжать process-delta только внутри того же `(pid, starttime)`;
- разделить provenance OSSC и vadv для Statement→Plan;
- обновить OpenAPI, TypeScript-схему, английский и русский README;
- добавить поведенческие тесты на PID reuse, неоднозначный PID и fork-specific plan attribution.

Критерий готовности: ни один публичный контракт не называет PID-only или time-only связь точной.

### PR 2. Корректные метрики и непрерывность счётчиков

Цель: сделать формулы reset-aware и привести названия к реальной семантике источников.

Изменения:

- единый continuity verdict для `reset`, `gap`, `first_point` и превышения `max_rate_gap_us`;
- CPU процесса с делением на `clock_ticks_per_sec`;
- `null` для нулевого или неизвестного знаменателя;
- отдельные формулировки для PostgreSQL buffer reads и storage-accounted bytes;
- `first_call` вместо выдуманного `first_seen`;
- lock wait age только при наличии `waitstart`, без «времени удержания»;
- раздельные поля vacuum progress для разных layout и версий.

Критерий готовности: golden-тесты формул покрывают reset, gap, нулевой знаменатель и несовместимые источники.

### PR 3. Общий forensic shell

Цель: построить постоянный каркас 1920×1080 без обязательной прокрутки первичного вывода.

Изменения:

- Global context, навигация Workload/Data/Host/Events и режим Live/Replay;
- единая Health line высотой 60 px;
- общий cursor, brush, selected range и baseline;
- prepared lens selector, status strip и semantic badges;
- синхронизация времени через один store;
- keyboard navigation, focus states и reduced motion;
- базовый `ProvenancePopover` и состояния `null`, partial, gated, reset и gap.

Критерий готовности: shell помещается в 1920×1080 и работает с клавиатуры при масштабе браузера 100 %.

### PR 4. Statements как ранжированная time matrix

Цель: дать основной экран расследования для примерно 1 000 строк `pg_stat_statements`.

Изменения:

- разрезы Workload, Latency, Buffers, WAL, Temp, Planning, Regression и Observed samples;
- 96-bucket heatmap на общей временной оси;
- sticky identity и 5–7 числовых столбцов;
- серверная пагинация и виртуализация строк;
- impact ranking, baseline delta и честное состояние `query text unavailable`;
- responsive fallback без потери основной таблицы.

Критерий готовности: 1 000 строк не создают 1 000 DOM-строк и не вызывают заметного input lag на эталонном экране.

### PR 5. Global forensic search и Entity Detail

Цель: превратить поиск и detail-панель в общий механизм расследования.

Изменения:

- command palette по `/`;
- поиск по PID, queryid, planid, relation, index, OID, wait, event, database, user, application, cgroup и device;
- группы результатов с причиной совпадения и provenance;
- detail drawer шириной 480–560 px без скачка основной таблицы;
- вкладки Summary, History, Relationships и Raw evidence;
- share state без SQL и других чувствительных свободных строк.

Критерий готовности: поиск работает по серверному продолжению, а не только по загруженной странице.

### PR 6. Activity и Plans

Цель: реализовать два связанных workload-экрана с разной аналитической геометрией.

Изменения:

- Activity: Overview, Waits & Locks, Duration, CPU, Disk I/O, Memory, Replication, XID/Horizon и Sampling;
- process-link badge с качеством связи и переходом в Process Detail;
- blocking tree и waiter lanes;
- Plans: Regression, Time, I/O & Buffers, Rows, Change timeline и Compare;
- fork-specific OSSC/vadv provenance;
- side-by-side plan tree diff и before/after evidence lanes.

Критерий готовности: activity остаётся point snapshot, а observed samples не называются точной стоимостью queryid.

### PR 7. OS, Tables, Indexes и Vacuum

Цель: объединить Linux pressure и data-object evidence в подготовленные разрезы.

Изменения:

- OS Pressure, CPU, Memory, Storage I/O, Network, Filesystems, Cgroups, Processes и Data quality;
- USE-oriented lanes без смешивания utilization, saturation и errors;
- Tables: Health, Vacuum risk, I/O, Scan pattern, Size growth, XID/MXID и Dependencies;
- Indexes: Usage, I/O, Growth, Duplication, Invalid/Build и Table context;
- Vacuum: Progress, Throughput, Phase, Blockers, Wraparound risk и History;
- reusable Entity Detail для процесса, таблицы, индекса и vacuum worker.

Критерий готовности: scope host/pod/container не складывается, а process/cgroup caps видны как `resource_limited`.

### PR 8. Events, доступность и production polish

Цель: завершить расследовательский цикл и подготовить интерфейс к production-использованию.

Изменения:

- Events timeline, Errors, Checkpoints, Autovacuum, Slow queries, Config changes и Collector health;
- переход Event→Entities с явным качеством каждой связи;
- cross-screen investigation set;
- accessibility audit, contrast, keyboard-only и screen-reader labels;
- performance budget, empty/error/loading states и визуальная регрессия 1920×1080;
- финальная проверка локализации и design tokens.

Критерий готовности: все восемь верхнеуровневых экранов используют один временной контракт, Health line, поиск и Detail.

## Общие ограничения

- Любая связь содержит `kind`, `method` и проверяемые поля provenance.
- Heatmap остаётся рядом с correlation и не заменяется correlation score.
- Все 64-битные идентификаторы передаются в браузер как строки.
- У каждой rate- и ratio-метрики есть политика reset, gap и нулевого знаменателя.
- Любой набор строк, bucket или поисковый результат имеет серверный cap и typed continuation.
- Новые структуры не растут без лимита. Ревью каждого PR отдельно оценивает peak memory.
- Комментарии объясняют инвариант или ограничение, а не пересказывают код.
- Публичные контрактные изменения синхронно обновляют `README.md`, `README.ru.md`, OpenAPI и TypeScript schema.
- UI проверяется на 1920×1080, 1440×900 и при keyboard-only navigation.

## Обязательные проверки каждого PR

```bash
cargo fmt --all --check
RUSTFLAGS="-D warnings" cargo clippy --workspace --all-targets
cargo test --workspace
cargo run -p xtask -- check-deps
make openapi
make web-frontend-check
```

Перед открытием PR проводится отдельное ревью корректности PostgreSQL, Linux performance/observability, memory bounds, комментариев, локализации и фокуса diff.
