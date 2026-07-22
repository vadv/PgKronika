# План закрытия оставшихся аналитических контрактов

Статус: нормативный план реализации.

## Назначение и границы

Этот документ задаёт порядок закрытия оставшихся контрактов анализа. Пока
источник данных не доказывает связь, соответствующий `EntityJoin` остаётся
недоступным; базовая ветка линзы продолжает работать без обогащения.

Текущее состояние:

- 28 основных линз и 14 веток событий зарегистрированы и достижимы в рабочем
  коде;
- 12 веток событий имеют собственные ID, ещё две используют `OS-FS-027` и
  `PG-CONN-014`;
- всего есть 40 уникальных стабильных ID и 42 ветки `evaluator`;
- стабильных ID в состояниях `dormant` и `catalog-only` нет;
- незакрыты строгие контракты `EntityJoin` у 24 линз.

Доступность `evaluator` в рабочем коде не означает, что его строгий контракт
доказательств закрыт. `catalog.applied` сообщает о регистрации базовой ветки,
но не доказывает готовность каждого необязательного `EntityJoin`, полноту
входных данных или завершённость оценки конкретного запроса.

Фактическое состояние определяют рабочий код, манифесты, реестр и
[архитектурные границы](../../architecture.md). Исторические
[проекты линз](../specs/2026-07-16-kronika-incident-lenses-design.md) и
[план реализации](../specs/2026-07-17-kronika-incident-implementation.md) дают
контекст, но не являются источником текущих контрактов. Этот документ задаёт
порядок будущих изменений.

## Обязательные инварианты

Прямая связь сущностей допустима только при одновременном выполнении всех
условий:

1. Совпадают `source_id` и `node_self_id`.
2. Совпадают `domain`, `name` и все компоненты идентификатора (`value`). Для
   backend обязательна как минимум пара `(pid, backend_start)`. Для relation
   или database одного OID недостаточно: нужны идентификатор базы данных и
   границы жизни объекта.
3. Связь доказана одним из трёх контрактов активации:
   - `SharedSnapshot`: `shared_snapshot_producer`,
     `shared_snapshot_token`, `both_inputs_complete`;
   - `SnapshotRelation`: `typed_relation_producer`,
     `snapshot_scoped_relation`, `relation_and_inputs_complete`;
   - `LifetimeMapping`: `stored_mapping_producer`,
     `overlapping_lifetime_mapping`, `mapping_and_inputs_complete`.
4. Известна полнота входных данных и самой relation или mapping. Если покрытие
   неизвестно, отсутствие строки не доказывает отсутствие связи.
5. Для `LifetimeMapping` пересечение считается по полуоткрытым интервалам.
   Равенство конца одного интервала началу другого не образует пересечения.

В каталоге объявлено 11 контрактов `SnapshotRelation`, 12
`LifetimeMapping` и один `SharedSnapshot`. Разделение ниже на группы 6/8/9/1
задаёт порядок закрытия по доступности входных данных.

Совпадение числовых значений при разных `kind` или `domain`, близкие метки
времени и принадлежность одному кластеру инцидента не доказывают relation.
Метка времени наблюдения не назначает `lead` или `downstream`; направление
разрешено только явным сохранённым структурным ребром.

Для producer, индекса и `evaluator` задаются лимиты на число входных связей,
объём работы, память, findings и evidence. Дедупликация и сортировка должны быть
детерминированными. Переполнение счётчиков и расчёта бюджета работы не должно
вызывать панику или обходить лимит. Квадратичный поиск до допуска по бюджету
работы запрещён.

В рабочем `EntityJoin` типизирована только сущность PostgreSQL
`BackendSession(pid, backend_start)`. Для relation, database, Linux process,
filesystem, device, cgroup и network endpoint сначала нужен полноценный тип с
полной identity; сравнение скалярных значений не заменяет этот шаг.

Активация учитывает ограничения версии и layout, записанные в registry.
Отсутствующее в этой версии поле остаётся missing и не заменяется нулём.
Приёмочные тесты покрывают PostgreSQL 15–18 и варианты extension layout,
которые меняют состав identity или evidence.

## Контракты на имеющихся исходных данных (`adapter-first`): 6

В этой группе нужные исходные строки уже сохраняются. Закрытие всё равно
требует типов identity в рабочем коде, provenance, coverage, ограниченного
индекса и подключения `evaluator`.

### `PG-TEMP-003` — `query_database_temp` (`SnapshotRelation`)

- **Есть:** `pg_stat_database(datid, temp_bytes, temp_files)`, идентификатор
  запроса `pg_stat_statements(queryid, userid, dbid)`, `toplevel` в layout, где
  поле объявлено, счётчики временных блоков запроса и `pg_log_temp_files`.
- **Не хватает:** доказанной Query-to-Database relation, общего snapshot
  provenance и полноты обеих групп входных данных. `pg_log_temp_files` не
  хранит `queryid`, `dbid` или backend identity и остаётся нетипизированным
  контекстом.
- **Минимальный путь:** сохранить ограниченную типизированную relation из
  строки statement, перенести её `source_id`, `node_self_id`, snapshot и
  coverage во вход инцидента, затем подключить evidence базы данных и запроса.
  Для direct log enrichment нужен отдельный typed producer/schema.
- **Приёмка:** сквозной тест с `dbid=42`, `dbid=43` и одинаковым `queryid`
  публикует temp evidence через доказанную связь только для точной identity;
  совпадение `queryid` между базами связи не создаёт.

### `PG-VACUUM-005` — `relation_vacuum` (`SnapshotRelation`)

- **Есть:** `pg_vacuum_observation` одной строкой хранит `datid`, `relid`, PID,
  `backend_start` и `query_start`.
- **Не хватает:** Relation/VacuumRun identity в рабочем коде, ограниченного
  relation index и явного признака полноты снимка.
- **Минимальный путь:** построить relation из той же producer row, сохранить
  identity базы данных, полный идентификатор backend, snapshot и coverage;
  данные журнала autovacuum пока использовать только как временной контекст.
  Прямую связь можно включить после того, как producer/schema сохранит `datid`,
  `relid` и identity `VacuumRun`, а не только полное имя relation.
- **Приёмка:** точная строка обогащает finding данными нужной relation; тот же
  PID с другим `backend_start` и тот же `relid` в другой базе данных не
  связываются.

### `PG-REPL-015` — `replication_wal` (`SnapshotRelation`)

- **Есть:** `pg_replication_physical` в одной строке хранит
  `(pid, backend_start)`, slot/type/state/sync state и WAL gap/stage.
- **Не хватает:** ReplicationSession/WalState identity в рабочем коде,
  проекции relation и coverage полного снимка.
- **Минимальный путь:** выпустить ограниченную relation из одной строки с
  `source_id`, `node_self_id`, snapshot и признаком полноты, затем подключить
  её к replication evidence.
- **Приёмка:** обогащение получает только точный экземпляр backend по
  `(pid, backend_start)`; строка с тем же PID и другим `backend_start` остаётся
  несвязанной.

### `PG-SYNC-018` — `backend_replication` (`SnapshotRelation`)

- **Есть:** activity содержит полную `(pid, backend_start)` identity и SyncRep
  wait; `pg_replication_physical` содержит ту же session identity и
  `sync_state`.
- **Не хватает:** snapshot-scoped relation между семействами входных данных и
  их coverage.
- **Минимальный путь:** создать ограниченный index из physical row и
  сопоставлять только полную session identity в том же source/node и
  доказанном snapshot.
- **Приёмка:** SyncRep finding получает replication evidence только при точном
  `backend_start`; сценарий повторного использования PID сохраняет базовый
  finding без evidence связи.

### `OS-BLOCK-024` — `pg_storage_block_device` (`LifetimeMapping`)

- **Есть:** `os_diskstats(major, minor)` и `pg_storage_mount` V2 с точными
  `major`, `minor`, `ts`, `role` и mount namespace.
- **Не хватает:** adapter сворачивает mount rows в общий набор запроса и теряет
  время, role и namespace; интервал mapping и coverage не построены.
- **Минимальный путь:** сохранить исходные rows, получить ограниченные интервалы
  действительности и связывать device только при пересечении в одном scope.
- **Приёмка:** пересечение публикует `postgres_storage_exact`; те же
  `major`/`minor` вне интервала или в другом mount namespace остаются
  недоказанными.

### `OS-FS-027` — `pg_storage_filesystem` (`LifetimeMapping`)

- **Есть:** `pg_storage_mount` хранит в одной row PG role/path hash, mount hash,
  namespace, capacity, `mapping_state` и timestamp.
- **Не хватает:** Path/Filesystem identity в рабочем коде, интервала mapping и
  coverage. Если продукту достаточно базовых данных из одной строки, лишний
  cross-section requirement нужно удалить явно, а не создавать фиктивный join.
- **Минимальный путь:** построить типизированный path-to-filesystem mapping с
  ограниченным интервалом жизни либо принять и проверить решение об удалении
  требования.
- **Приёмка:** проверенный пересекающийся mapping даёт evidence точной файловой
  системы; тот же hash в другом namespace и касание полуоткрытой границы не
  связываются.

## Частично имеющиеся входные данные без доказанного provenance: 8

Здесь часть данных уже есть, но она не доказывает объявленную relation.
Временная корреляция и множество кандидатов не заменяют недостающий producer.

### `PG-FREEZE-006` — `relation_vacuum_horizon` (`SnapshotRelation`)

- **Есть:** `pg_freeze_horizon` уже хранит relation и horizon в одной строке, а
  связь relation с vacuum находится в `pg_vacuum_observation`.
- **Не хватает:** `relation_vacuum_horizon` не определяет точный состав связи;
  provenance и coverage вариантов также не определены.
- **Минимальный путь:** сначала выбрать точный состав связи. Для
  Relation-to-Horizon достаточно типизировать существующую строку и её coverage;
  для VacuumRun-to-Horizon нужен новый общий producer. Ненужный составной
  requirement следует разделить или удалить.
- **Приёмка:** выбранная explicit relation связывает точные `datid` и `relid`;
  тот же `relid` в другой базе данных или snapshot — нет. Тест отдельно
  фиксирует выбранную семантику endpoints.

### `PG-IO-011` — `pg_io_block_device` (`LifetimeMapping`)

- **Есть:** `pg_stat_io(backend_type, object, context)` начиная с PostgreSQL 16,
  точный набор PG storage devices и `os_diskstats`.
- **Не хватает:** у `pg_stat_io` нет атрибуции по устройствам; принадлежность к
  storage не доказывает, какой device обслужил I/O.
- **Минимальный путь:** добавить explicit PG-I/O mapping producer для каждого
  device с интервалом жизни и coverage либо сузить вопрос до контекста всего
  экземпляра без прямой атрибуции. Для PostgreSQL 15 requirement остаётся
  unavailable.
- **Приёмка:** совпавшие по времени `pg_stat_io` и `diskstats` без mapping не
  дают evidence связи; явный пересекающийся mapping связывает только указанное
  устройство.

### `PG-SLOT-016` — `slot_filesystem` (`LifetimeMapping`)

- **Есть:** snapshot слота со `slot_name`, type, `active_pid` и retained WAL, а
  также `pg_storage_mount` с ролью WAL.
- **Не хватает:** стабильного идентификатора поколения и интервала жизни слота,
  а также slot-to-filesystem row.
- **Минимальный путь:** определить identity поколения слота и хранить
  ограниченные интервалы его связи с WAL filesystem вместе с coverage.
- **Приёмка:** удалённый и созданный заново слот с тем же `slot_name` не
  наследует старый mapping; связывается только пересекающееся поколение.

### `OS-CPU-020` — `host_pg_cpu` (`LifetimeMapping`)

- **Есть:** host `os_cpu`/PSI, `os_process(pid, starttime, cpu)` и PostgreSQL
  activity `(pid, backend_start)`.
- **Не хватает:** связи `BackendSession` с OS process на заданном интервале и
  полноты перечня процессов.
- **Минимальный путь:** сохранить каноническую типизированную relation PG
  session-to-OS process в одном source/node, затем доказать принадлежность узлу
  и coverage.
- **Приёмка:** обогащение CPU появляется для точного интервала жизни process;
  тот же PID с другим start или строка другого node не связываются.

### `OS-CGRP-021` — `backend_cgroup_cpu` (`LifetimeMapping`)

- **Есть:** `os_cgroup_cpu`, `os_cgroup_mapping(pid, starttime, ts,
  cgroup_path, scope)`, process и activity rows.
- **Не хватает:** `os_cgroup_mapping` не доказывает соответствие
  `BackendSession(pid, backend_start)` процессу ОС. Adapter не загружает mapping
  как ограниченный дополнительный вход и не сохраняет `scope`. Интервалы и
  coverage также отсутствуют.
- **Минимальный путь:** добавить канонический PG-session-to-OS-process bridge,
  загружать mapping в пределах бюджета, сохранить `scope` во всех identity и
  index keys и построить cgroup intervals с coverage.
- **Приёмка:** точная session получает CPU evidence своего cgroup; повторное
  использование PID, несовпавший `backend_start` и тот же path другого source
  или `scope` связи не создают.

### `OS-MEM-022` — `host_pg_memory` (`LifetimeMapping`)

- **Есть:** `os_meminfo`, RSS/swap из `os_process` и activity.
- **Не хватает:** полного проверенного перечня PG processes на узле. Частичный
  перечень не доказывает агрегат PG memory.
- **Минимальный путь:** получить ограниченный набор интервалов жизни всех PG
  processes, coverage этого набора и доказанную принадлежность узлу.
- **Приёмка:** complete mapping даёт агрегированное PG memory evidence; пропуск
  одной mapping row или неизвестное coverage оставляет requirement unavailable
  и не создаёт прямой атрибуции.

### `OS-CGMEM-023` — `backend_cgroup_memory` (`LifetimeMapping`)

- **Есть:** проверенная `pg_process_cgroup_memory` row, относящаяся только к
  соединению коллектора, а также generic process/cgroup/activity rows.
- **Не хватает:** строка collector session не доказывает cgroup произвольного
  наблюдаемого backend; нет канонической relation интервала жизни backend.
- **Минимальный путь:** расширить schema/producer до полной backend-session
  identity, интервала mapping и completeness.
- **Приёмка:** при двух backends evidence получает только явно mapped экземпляр;
  cgroup соединения коллектора не переносится на второй backend.

### `OS-IOWHO-026` — `process_cgroup_device` (`LifetimeMapping`)

- **Есть:** `os_process` I/O, `os_cgroup_mapping`,
  `os_cgroup_io(cgroup_path, major, minor, scope)` и consumer
  `associated_device`.
- **Не хватает:** mapping не загружается автоматически, поиск выполняется
  только для одной метки времени, а сохранённый интервал действительности и
  coverage отсутствуют; adapter также теряет сохранённый `scope` обеих секций.
- **Минимальный путь:** загрузить ограниченный дополнительный input, построить
  interval mapping, провести `scope` через identity/index и сохранить
  существующие work/output limits.
- **Приёмка:** точный интервал process публикует `cgroup_device_association` с
  ожидаемыми `major`/`minor`; повторное использование PID, касание временной
  границы, другой `scope` и отсутствие overlap оставляют только process
  evidence.

## Контракты, которым нужен новый producer связи или расширение schema: 9

Эти контракты нельзя закрыть только подключением adapter. Сначала нужен
producer, который хранит обе типизированные identity, provenance и coverage.
Иначе нужно явное продуктовое решение оставить direct relation недоступной.

### `PG-ANALYZE-004` — `relation_query_plan` (`SnapshotRelation`)

- **Есть:** table stats `(datid, relid, n_mod_since_analyze, reltuples)` и
  `PlanSample(dbid, userid, queryid, planid)`.
- **Не хватает:** связи OID relation с query/plan.
- **Минимальный путь:** новый ограниченный producer общего снимка с Database,
  Relation, Query и Plan identities и coverage.
- **Приёмка:** explicit row обогащает только свой plan; равные скалярные ID в
  другой базе данных или domain без row не связываются.

### `PG-HOT-007` — `relation_index_wal` (`SnapshotRelation`)

- **Есть:** table/index rows с `datid`, `relid`, `indexrelid`, счётчики non-HOT,
  global и per-query WAL counters.
- **Не хватает:** relation/index-to-WAL attribution.
- **Минимальный путь:** добавить explicit WAL producer для relation/index или
  оставить requirement в состоянии `unavailable`.
- **Приёмка:** одновременные всплески non-HOT и WAL без relation не связываются;
  типизированная producer row обогащает только mapped relation/index.

### `PG-WAL-009` — `query_wal_checkpoint` (`SnapshotRelation`)

- **Есть:** global `pg_stat_wal`, per-query statement WAL в layout, где эти
  колонки объявлены, и входы checkpointer/log.
- **Не хватает:** direct или causal query-to-checkpoint relation; совпадение
  времени её не доказывает.
- **Минимальный путь:** выпускать только explicit producer relation. Если такой
  producer не определён, сузить или отложить контракт.
- **Приёмка:** пересекающиеся query WAL и checkpoint без relation остаются
  несвязанными; сохранённая типизированная relation в доказанном snapshot даёт
  обогащение.

### `PG-CACHE-010` — `relation_query_cache` (`SnapshotRelation`)

- **Есть:** relation heap/index block counters и database/query block counters.
- **Не хватает:** relation-to-query mapping.
- **Минимальный путь:** ограниченный producer с Database, Relation и Query
  identities, snapshot provenance и coverage.
- **Приёмка:** общие database/time и равные числа сами по себе не связывают
  rows; explicit mapping связывает ровно одну пару Query–Relation.

### `PG-HORIZON-013` — `backend_relation_horizon` (`SnapshotRelation`)

- **Есть:** activity с полной session identity и `backend_xmin_age`, relation
  freeze horizons и отдельный database-level aggregate `pg_prepared_xacts`.
- **Не хватает:** нет прямой связи backend с relation, горизонт которой он
  удерживает. Агрегат prepared transactions этого не доказывает; нужные данные
  replication slots и standby feedback сейчас не собираются. Lock enrichment
  относится к отдельному контракту общего снимка.
- **Минимальный путь:** producer row
  `(BackendSession, Database, Relation, horizon kind/value)` с coverage либо
  решение оставить только backend baseline.
- **Приёмка:** высокий xmin и старая relation в одно время без producer row не
  связываются; точная relation полной session связывается, повторно
  использованный PID — нет.

### `PG-CONN-014` — `database_backend` (`SnapshotRelation`)

- **Есть:** `pg_stat_database(datid, numbackends, datconnlimit)`; activity
  содержит `datname` и полную backend session identity.
- **Не хватает:** стабильного `datid` в activity relation. Имя базы данных не
  образует достаточной identity.
- **Минимальный путь:** добавить `datid` в registry/source schema activity или
  сохранить dedicated database-to-session relation из одной строки.
- **Приёмка:** join использует `datid` и полный идентификатор backend;
  переименование или совпадение имён, OID reuse, другой source/node или snapshot
  не связываются.

### `PG-ARCH-017` — `archive_filesystem` (`LifetimeMapping`)

- **Есть:** счётчики archiver, настройки `archive_command`/`archive_library` и
  сопоставления локальных хранилищ.
- **Не хватает:** доказанного назначения архива; команда может писать в канал,
  удалённый сервис или произвольный процесс.
- **Минимальный путь:** producer разрешает локальное назначение в интервал жизни
  точки монтирования. Для удалённой или непрозрачной команды requirement
  остаётся `unavailable`.
- **Приёмка:** разрешённый local archive path связывается; `ssh`, pipe,
  удалённая команда и совпавший по времени полный filesystem — нет.

### `OS-WB-025` — `writer_block_device` (`LifetimeMapping`)

- **Есть:** global Dirty/Writeback из `os_meminfo` и `os_diskstats`.
- **Не хватает:** writer или BDI-to-device relation.
- **Минимальный путь:** ограниченный writeback producer для каждого BDI/device
  с интервалом жизни, loss accounting и coverage.
- **Приёмка:** dirty spike и нагруженное устройство без mapping не связываются;
  точная сохранённая BDI-device relation обогащает только своё устройство.

### `OS-NET-028` — `pg_endpoint_network` (`LifetimeMapping`)

- **Есть:** activity `client_addr`, netdev counters и PostgreSQL listen settings.
- **Не хватает:** endpoint/socket/route-to-interface relation с network
  namespace и lifetime.
- **Минимальный путь:** explicit route/socket mapping producer с endpoint
  identity, namespace, interval и coverage.
- **Приёмка:** точный mapping связывает interface; одинаковые значения адреса и
  интерфейса в другом namespace/source и временное совпадение не связываются.

## Отдельный блокер общего снимка (`shared snapshot`): 1

### `PG-WAIT-019` — `activity_lock_waiter` (`SharedSnapshot`)

- **Есть:** activity snapshots, `pg_locks` edges с waiter
  `(pid, backend_start)` и типизированный ограниченный consumer/index для lock
  enrichment.
- **Не хватает:** collectors читают activity и locks разными statements;
  adapter поэтому не выдаёт общий snapshot token. Совпадение меток времени само
  по себе не доказывает общий снимок.
- **Минимальный путь:** один producer получает обе группы данных из одного
  доказанного снимка, присваивает ему общий token и сохраняет
  `both_inputs_complete`. Adapter без преобразования смысла передаёт token в
  index запроса.
- **Приёмка:** положительный сквозной тест storage-to-engine-to-HTTP с точным
  token, scope и полной session identity публикует детерминированные `Direct`
  lock edges. Отрицательные сценарии без token, с другой меткой времени,
  повторным PID, несовпавшим `backend_start`, другим source/node, malformed
  endpoint или неполным coverage не дают relation. Повторяющиеся blockers
  дедуплицируются, допуск по бюджету работы соблюдается, evidence ограничен 128.

## Общий контракт приёмочных тестов

Для каждого активируемого join нужен детерминированный положительный сквозной
тест storage-to-`GET /v1/incidents`. Он должен показывать evidence доказанной
связи и доступность requirement, а не только существование базового finding.

Парные отрицательные тестовые сценарии по одному меняют:

- source или node;
- identity domain, name или полное value;
- snapshot token или snapshot-scoped relation;
- границу интервала mapping, включая полуоткрытую границу;
- PID/`backend_start`, database/relation lifetime или namespace;
- coverage входов или relation/mapping.

Отдельно проверяются совпадения числовых значений в разных доменах, `null` и
частичная identity, повторяющиеся строки, детерминированное разрешение равенств,
checked overflow, исчерпание бюджета работы и лимиты памяти, findings, output и
evidence. Тесты не используют sleep и не выводят причинность из меток времени.

## Честность HTTP API

### Показать требования активных линз

`catalog.dormant` не может быть единственным местом для prerequisites: при
нулевом числе dormant он скрывает все 24 незакрытых join. Ответ должен
показывать для каждого активного основного ID:

- машинный ID требования;
- имя контракта и тип активации;
- статус каждого условия producer, provenance и coverage для текущего запроса;
- причину недоступности из закрытого набора машинных значений.

Статус нужно получать из реально вызванной ветки evaluator и подготовки
текущего запроса. Нельзя целиком публиковать `DORMANT_CATALOG.missing` как
текущее состояние: generic tokens для counters, gauges, activity и paired
intervals частично или полностью реализованы. Этот план повторно подтверждает
только 24 строгих `EntityJoin`; остальные requirements требуют отдельной
runtime-проверки. Перед проекцией `DORMANT_CATALOG` и `DormantLens` следует
переименовать в нейтральные requirements metadata и сверить их
`confidence_cap` с фактическими evaluator branches.

Схема OpenAPI должна иметь закрытые enum и схемы объектов. Положительный и
missing-сценарии должны проверять HTTP-проекцию: `kind` требования и
`domain`/`name`/`value` identity, а также отсутствие локализованного текста.
Глобальный status нельзя объявлять complete, пока обязательное применимое
условие missing. `requirements_status` должен вычисляться из этих условий для
текущего запроса, а не из регистрации линзы.
Проверка должна охватывать tracked OpenAPI artifact и фактический HTTP route;
если спецификация не обслуживается по HTTP, документация не должна обещать
такой route.

### Исправить `diagnosis_available`

Поле не должно вычисляться из непустого списка зарегистрированных линз.
`diagnosis_available=true` допустимо, если хотя бы один `evaluator` фактически
допущен на данных текущего запроса; наличие finding не требуется. Ответы,
завершённые на `no_data`, missing или conflicting identity до dispatch, должны
возвращать `false`. Если клиентам нужна отдельная видимость каталога, для неё
следует ввести поле `catalog_available`, не перегружая `diagnosis_available`.

### Разделить зарегистрированные и выполненные ID

Ответ анализа событий должен хранить полный ограниченный список
зарегистрированных веток отдельно от ID, которые `evaluator` действительно
выполнил. Досрочное завершение, отказ по бюджету и лимит output/work не должны
записывать неисполненный ID в `evaluated_lens_ids`. Нужны HTTP-тесты для
пустого, досрочного, полного и ограниченного бюджетом проходов.
`evaluation_complete=true` допустимо только после выполнения всех допущенных
веток без остановки по work/output limits и без неизвестного coverage.

### Синхронизировать числа и терминологию

Документация и проверяемый API contract должны использовать 14 веток событий,
40 уникальных стабильных ID и 42 ветки `evaluator`. Устаревшие 8 веток событий
и 34 уникальных ID в
[руководстве по incident analysis](../../incident-analysis.md) нужно заменить.
Утверждения о `partly dormant` catalog должны описывать active requirements, а
не несуществующие catalog-only ID. Числа следует получать из каталогов линз
или проверять против них, чтобы расхождение приводило к ошибке CI. Область
каждого счётчика должна быть явной: для уникальных стабильных ID это 40 active
и 0 inactive, для evaluator branches — 42. Клиент не должен выводить inactive
count из пустого массива.

## Оставшиеся задачи вне линз инцидентов

Эти задачи не меняют число dormant ID и не являются условием активации
конкретного `EntityJoin`.

### Полнота diff

Одиночный и пакетный diff должны явно отдавать ограниченные gaps, coverage и
completeness. Окно без пары точек по обе стороны разрыва и пустой ряд не могут
молча выглядеть полными. Полный интервал с нулевой разностью остаётся полным и
отличается от отсутствующих данных. Формальная семантика pair/gap описана в
[контракте diff](../specs/2026-07-14-kronika-diff-design.md).

### Усечение anomaly

Итоговое ранжирование должно учитывать episodes, отброшенные после объединения
sections, и отдавать общий status, completeness и числа усечений. Пропущенный
section, отказ по бюджету работы и любое усечение запрещают `complete=true`.
Ранжирование остаётся ограниченным и детерминированным; требования к границе
episode находятся в
[контракте anomaly](../specs/2026-07-15-kronika-anomaly-design.md).

### Аналитический интерфейс

Статическая HTML-заглушка и JSON probes не дают функционального аналитического
интерфейса.
Нужно принять одно из двух решений:

1. реализовать доступные Overview, Health и Index views с явными partial,
   error и coverage states и browser/BDD-проверками;
2. исправить README и описание продукта, явно оставив только static shell и API.

Локализованные подписи принадлежат UI; HTTP-ответы содержат только машинные
значения по
[контракту языка API](../specs/2026-07-21-i18n-machine-api-contract.md).

## Порядок реализации

1. **P0 — наблюдаемость.** Показать требования активных линз, исправить
   `diagnosis_available`, разделить зарегистрированные и выполненные ID,
   проверять числа 14/40/42.
2. **P1 — общий фундамент.** Ввести типизированные identities в рабочий код для
   нужных domains, единый ограниченный допуск relation/mapping и статус coverage
   для текущего запроса.
3. **P2 — имеющиеся исходные данные.** Закрывать `PG-VACUUM-005`,
   `PG-REPL-015`, `PG-SYNC-018`, `PG-TEMP-003`, затем `OS-BLOCK-024` и
   `OS-FS-027`, для которых нужно сохранить временные границы.
4. **P3 — общий снимок.** Добавить общий activity/locks producer и только после
   этого включить lock enrichment `PG-WAIT-019`.
5. **P4 — частичные входные данные.** Сначала протянуть ограниченные OS mapping
   inputs, затем определить недостающие PG-session/process/slot/device contracts
   для восьми частично обеспеченных ID.
6. **P5 — новые relations.** Для девяти оставшихся ID принять domain contract и
   реализовать producer. Вопросы без доказуемой relation остаются unavailable
   либо их requirement сужается отдельным продуктовым решением.
7. **P6 — полнота вне линз.** Закрыть status и truncation для diff/anomaly,
   затем выполнить решение по аналитическому интерфейсу.

## Стоп-условия

Активация конкретного join останавливается, если выполняется хотя бы одно
условие:

- для `SharedSnapshot` один producer не выдаёт общий token, точную identity и
  `both_inputs_complete` для обеих групп входных данных;
- для `SnapshotRelation` нет ограниченной producer row с обоими typed endpoints,
  snapshot provenance и `relation_and_inputs_complete`;
- для `LifetimeMapping` нет сохранённого mapping обоих endpoints,
  полуоткрытого пересечения интервалов и `mapping_and_inputs_complete`;
- scope или полная identity неизвестны;
- положительный сквозной тест storage-to-HTTP отсутствует;
- отрицательный сценарий совпадения или повторного использования образует
  relation;
- лимит входа, работы, памяти, output или evidence не доказан;
- детерминированные порядок и дедупликация либо checked overflow не проверены;
- направление связи выведено только из метки времени;
- для активации пришлось бы объявить неполные данные полными.

Цель — не свести число missing requirements к нулю любой ценой. Контракт без
достаточных входных данных должен оставаться точно и наблюдаемо unavailable.

## Критерии завершения

Работа по контракту завершена, когда одновременно выполнено следующее:

- ни одна direct relation не получается из простого равенства числовых значений;
- source, node, snapshot/provenance и полная типизированная identity проверяются
  на границах producer, adapter, index и `evaluator`;
- lifetime reuse, PID reuse, OID reuse, namespace и полуоткрытые границы
  покрыты регрессионными тестами с отрицательными сценариями;
- работа, память, findings, output и evidence ограничены; overflow не вызывает
  панику и не обходит admission;
- evidence связи доступно через HTTP; этот путь покрыт точечными модульными
  тестами, тестами HTTP-обработчика и BDD-сценариями там, где путь затрагивает
  storage;
- requirements status и evaluation completeness честно отражают конкретный
  запрос;
- числа в документации и машинные ID получают из каталогов линз либо
  проверяют против них;
- связанные изменения согласованы с
  [архитектурными границами](../../architecture.md),
  [PostgreSQL registry](../../type-registry/postgresql.md),
  [OS registry](../../type-registry/os.md) и
  [семантикой колонок](../../type-registry/semantics.md).
