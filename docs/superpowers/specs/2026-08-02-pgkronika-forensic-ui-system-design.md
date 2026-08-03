# PgKronika: система forensic-экранов для PostgreSQL и Linux

Дата: 2 августа 2026 года  
Статус: дизайн-концепция для проверки на макетах  
Аудитория: DevOps-инженеры, SRE и DBA, которые восстанавливают ход инцидента по историческим данным  
Базовый экран: 1920×1080, без обязательной вертикальной прокрутки для первичного вывода

## 1. Результат

PgKronika должна выглядеть не как набор представлений `pg_stat_*`, а как единый инструмент расследования. Экран отвечает на вопрос оператора, совмещая PostgreSQL, ОС, события и качество данных. Верхнеуровневый раздел задаёт объект расследования, а подготовленный разрез — вопрос к данным.

Во всех разделах сохраняются:

- один выбранный момент времени или интервал;
- одна Health line для PostgreSQL и ОС;
- один курсор, выделенный диапазон и необязательный baseline;
- одинаковая шкала времени в графиках, heatmap и событиях;
- общий поиск по сущностям и свидетельствам;
- единая модель перехода в Entity Detail;
- явное различение причинной связи, временного совпадения и недоступной связи.

Экраны верхнего уровня:

1. OS.
2. Activity.
3. Statements.
4. Plans.
5. Tables.
6. Indexes.
7. Vacuum.
8. Events.

Это не восемь изолированных вкладок. Каждый экран использует связанные данные других разделов, но сохраняет собственный вопрос, ранжирование и смысловой центр.

## 2. Цели и нецели

### 2.1. Цели

- Дать максимум полезных свидетельств на 1920×1080 без визуального шума.
- Помочь за 10–20 секунд определить подозрительный интервал и главные сущности.
- Сохранить heatmap как основной способ увидеть совместное изменение множества сущностей.
- Не выдавать совпадение по времени за доказанную причинность.
- Работать с примерно 1 000 строк `pg_stat_statements` без потери ориентации.
- Сделать поиск самостоятельным инструментом расследования, а не фильтром уже загруженной таблицы.
- Показывать происхождение, окно, агрегацию, покрытие и ограничения каждого вывода.

### 2.2. Нецели

- Отображать все доступные метрики одновременно.
- Заменять SQL-профилировщик точной атрибуцией CPU или I/O к `queryid`, если источник этого не позволяет.
- Строить красивые, но семантически ложные p95, ETA, cache hit или causal-графы.
- Делать отдельный дашборд из карточек для каждого сборщика.
- Скрывать пропуски, reset, top-N truncation и отключённые настройки сбора.

## 3. Термины

| Понятие | Термин в документе | Точное значение |
| --- | --- | --- |
| Аналитический срез | Разрез | Подготовленная композиция графиков, heatmap и таблицы для одного вопроса. |
| Composite health timeline | Health line | Постоянная сводная линия здоровья PostgreSQL и ОС с событиями, пропусками и общим курсором. |
| Entity-by-time heatmap | Heatmap | Матрица «сущность × время», которая показывает совместное изменение и сохраняет связь на глаз. |
| Time-aligned association | Временное совпадение | Сигналы изменились в одном интервале, но причинная связь не доказана. |
| Exact link | Точная связь | Collector сохранил проверяемый relation token или обе сущности получены одним атомарным запросом источника. |
| Lifetime link | Связь по lifetime | PID попал в единственный доказанный lifetime того же boot и PID namespace. |
| Best-effort link | Приближённая связь | Связь восстановлена по неполному или зависящему от расширения ключу. |
| Data provenance | Происхождение данных | Источник, снимок или окно, агрегация, покрытие, reset и ограничения значения. |
| Baseline | Базовый интервал | Момент или интервал для сравнения с выбранным диапазоном. |

В интерфейсе можно оставить английские короткие названия разделов и разрезов. Подсказки и ограничения должны быть локализуемыми.

## 4. Общая архитектура

### 4.1. Группы навигации

| Группа | Разделы | Вопрос |
| --- | --- | --- |
| Workload | Activity, Statements, Plans | Что выполнялось и как изменилось поведение запросов? |
| Data | Tables, Indexes, Vacuum | Какие объекты данных создавали нагрузку или риск? |
| Host | OS | Где возник дефицит ресурса и какие процессы его сопровождали? |
| Events | Events | Какие дискретные события совпали с изменением системы? |

Processes и Locks не требуют постоянных верхнеуровневых вкладок. Они становятся подготовленными разрезами и типами Entity Detail внутри OS и Activity, оставаясь доступными из поиска.

До появления составного OS-экрана пункт Host / OS честно открывает существующее process-backed представление. Прямые ссылки на `processes` и `locks` сохраняются, но отдельные постоянные пункты навигации для них не возвращаются.

### 4.2. Постоянный каркас 1920×1080

| Область | Высота | Назначение |
| --- | ---: | --- |
| Global context | 44 px | Инстанс, база, роль, качество данных, инциденты, время, поиск `/`, ссылка. |
| Primary navigation | 32 px | Workload, Data, Host, Events; Live/Replay; 15m/1h/6h/24h. |
| Health line | 60 px | Здоровье PostgreSQL + ОС, покрытие, gap, события, brush, cursor, baseline. |
| Screen header | 68–76 px | Раздел, разрезы, источник, окно, покрытие, главный итог. |
| Analytical center | 220–260 px | 2–4 синхронных evidence lanes или смысловая визуализация. |
| Ranked matrix | 540–600 px | Плотная таблица или heatmap-матрица с 16–21 видимой строкой. |
| Status strip | 24 px | Режим курсора, свежесть, выбор, краткие клавиатурные подсказки. |

Entity Detail открывается поверх правой части на 480–560 px. Основная таблица не должна непредсказуемо менять ширину. Полноэкранный Detail используется для глубокого сравнения, дерева плана и истории объекта.

Реализованный viewport-контракт проверяется настоящим Chromium при 1920×1080, DPR 1 и 100% zoom командой `npm --prefix web run verify:shell` или make-обёрткой `make web-shell-check`. Проверка измеряет фиксированные области, отсутствие root-scroll, независимый overflow ranked matrix, минимум 16 полностью видимых строк, последовательную клавиатурную достижимость и режим reduced motion; диагностический и утверждённый снимки сохраняются в игнорируемом `web/demo/shots/`.

### 4.3. Правила плотности

- Строка таблицы по умолчанию — 28 px, компактный режим — 25–26 px.
- Шапка таблицы остаётся видимой; первый столб с идентификатором закреплён.
- Столбцы группируются по источнику: PostgreSQL, связь, ОС, derived.
- Результаты ранжируются по impact текущего разреза, а не по алфавиту.
- Слабые и здоровые значения остаются нейтральными. Цвет появляется на отклонении, выборе или доказательстве.
- На одном экране используется одна основная heatmap. Мини-графики допустимы только при общей шкале и окне.
- Большие KPI-карточки, gauge, donut, gradient и декоративные тени не используются.

## 5. Время, связь и достоверность

### 5.1. Единая временная геометрия

Все временные панели получают один диапазон и одинаковые координаты:

- Health line;
- evidence lanes;
- heatmap;
- события и интервалы;
- cursor;
- baseline;
- collector gaps.

Наведение на bucket подсвечивает тот же bucket в каждой видимой панели. Brush обновляет все данные после короткого debounce, но выделение двигается без задержки.

### 5.2. Грамматика связей

| Вид связи | Обозначение | Допустимая формулировка |
| --- | --- | --- |
| Точная | Сплошная линия или значок звена | «Связано по collector relation token». |
| По lifetime | Сплошная тонкая линия и метка `lifetime` | «PID попадает в единственный доказанный lifetime того же boot и namespace». |
| Временное совпадение | Пунктир | «Наблюдалось в том же интервале». |
| Приближённая | Точечная линия и метка `best effort` | «Вероятное соответствие; зависит от варианта `pg_store_plans`». |
| Связь отсутствует | Разрыв или `—` | «Источник не даёт ключа для связи». |

Корреляция может применяться для ранжирования подсказок, но не заменяет сырые сигналы. По этой причине heatmap сохраняется даже там, где есть correlation score.

### 5.3. Обязательная provenance

Любое вычисленное или выделенное значение раскрывает:

- источник;
- момент снимка или интервал;
- агрегацию;
- единицу;
- покрытие;
- reset boundary;
- sampling caveat;
- threshold rule и revision, если есть verdict;
- причину `null`, partial или gated.

## 6. Поиск

### 6.1. Два режима

1. **Lens filter** фильтрует текущий серверный набор и сохраняет контекст разреза.
2. **Global forensic search** открывается по `/` и ищет по всем доступным сущностям и свидетельствам.

Поиск не фильтрует только уже загруженные строки. Сервер возвращает точное число совпадений, продолжение выборки и причину совпадения.

### 6.2. Синтаксис

Поддерживаемые или проектируемые ключи:

```text
pid:18422
queryid:812774
planid:pl-9f
rel:public.orders
index:public.orders_created_idx
oid:16402
wait:DataFileRead
event:checkpoint
severity:error
db:erp_prod
user:api
app:web
cgroup:/kubepods.slice/...
device:8:0
```

Свободный текст ищет только в материализованных полях: сообщении события, `cmdline` и человекочитаемом имени сущности. Production collector для `pg_stat_statements` сохраняет server-truncated query text с лимитом `KRONIKA_PG_MAX_QUERY_TEXT`, но поле остаётся lazy/detail-only и не входит в bounded frame search; PostgreSQL может вернуть `null` для чужой роли. Plan text также остаётся lazy-полем. Текущий серверный контракт поддерживает до 16 AND-термов, равенство полей, case-insensitive glob `*`/`?` и типизированное равенство; операторы `cpu>80`, `duration>1s`, OR/NOT, диапазоны и `has:null` считаются отдельным будущим требованием, а не существующей возможностью.

### 6.3. Область поиска

- Current snapshot.
- Selected range.
- All retained data.
- Current lens.
- All entities.

### 6.4. Результат

Результаты группируются по Activity, Statements, Plans, Relations, Indexes, Processes, Vacuum и Events. Каждая строка показывает:

- тип и главный идентификатор;
- почему найдено совпадение;
- время или окно;
- состояние и impact;
- короткий фрагмент;
- происхождение данных;
- действие: открыть в разрезе, сравнить с baseline, добавить к расследованию.

SQL и другие чувствительные свободные строки не попадают в share URL. Идентификатор сущности, диапазон, разрез и сортировку можно сериализовать.

## 7. OS

### 7.1. Главный вопрос

Какой ресурс был ограничен, где возникло насыщение и какие процессы или PostgreSQL-сигналы наблюдались в том же интервале?

### 7.2. Разрезы

| Разрез | Смысловой центр | Основное ранжирование |
| --- | --- | --- |
| Pressure | USE: utilization, saturation, errors по CPU, memory и I/O | Ресурс по длительности и силе pressure. |
| CPU | CPU modes, load на ядро, run queue, CPU PSI, steal | Процесс или cgroup по CPU time в окне. |
| Memory | available, anon, file cache, dirty, writeback, swap, scan, faults, OOM | Процесс или cgroup по RSS/PSS и приросту. |
| Storage I/O | throughput, latency, queue, utilization, I/O PSI | Устройство, cgroup и процесс по фактическим байтам и задержке. |
| Network | rates, drops, errors, retransmits | Интерфейс и соединение по ошибкам или объёму. |
| Filesystems | capacity, inode, slope, time-to-full, mapping PG paths | Mount по риску заполнения и приросту. |
| Cgroups | throttle, memory high/max/OOM, I/O, pids | Cgroup по ограничению и affected processes. |
| Processes | CPU, memory, I/O, state, threads, cgroup, PG link | Процесс по impact выбранной метрики. |
| Data quality & topology | permissions, caps, scopes, clocks, boot epoch, devices и mounts | Источник по потерянному покрытию и риску ложного вывода. |

### 7.3. Композиция Pressure

- Health line остаётся единственным верхним агрегатом.
- Evidence lanes: CPU saturation, memory pressure, storage pressure и error/event lane.
- Heatmap: строки — CPU, memory, devices, cgroups или процессы; метрика переключается без изменения оси времени.
- Ranked matrix: resource, utilization, saturation, errors, Δ baseline, coverage, top contributor.
- При выборе процесса Detail совмещает `/proc`, cgroup и `pg_stat_activity`, если collector сохранил relation token или доказан однозначный lifetime. Совпадение одного PID всегда остаётся связью `best_effort`.

`rchar`/`wchar` показываются как логические байты, которые могут обслуживаться page cache. `read_bytes`/`write_bytes` показываются как storage-accounted bytes. Разность может называться только приблизительной оценкой cache-served I/O, но не «page-cache hits».

`SnapshotFull` означает полный результат модуля, но не гарантирует полное наблюдение системы. Лимит процессов в 4 096 строк и лимиты cgroup должны отображаться как `resource_limited` с направлением возможного selection bias. Host, pod network и container network остаются разными scopes и не складываются. PID→cgroup собирается реже, чем process metrics, поэтому миграция процесса может разорвать связь.

Каждая OS-метрика получает короткий semantic badge: `G` — gauge, `ΔC` — counter delta, `R` — rate, `S` — snapshot, `E` — event, `EST` — estimate. Badge раскрывает формулу и не заменяет единицу. Load average не называется CPU utilization; PSI не называется utilization; `io_time_ms / dt` не называется device saturation, а `io_weighted_time_ms / dt` — utilization.

OS evidence inspector всегда разделяет: «Наблюдалось», «Совпало с», «Возможный механизм», «Альтернативы», «Не доказывает», «Качество данных». PostgreSQL и OS lanes имеют общий cursor, но не общую шкалу значений.

## 8. Activity

### 8.1. Главный вопрос

Что выполнялось в выбранном снимке, чего ожидали backend-процессы, с какими процессами ОС они могли быть связаны и насколько надёжна эта связь?

### 8.2. Разрезы

| Разрез | Смысловой центр | Основное ранжирование |
| --- | --- | --- |
| Overview | PostgreSQL context + качество связи по PID + OS process metrics | Query age, wait duration или CPU. |
| Waits & Locks | Wait classes, blocking tree, waiter lanes | Суммарное blocked time и число downstream waiters. |
| Duration | Query age, transaction age, state timeline | Длительность запроса или транзакции. |
| CPU | Backend CPU, run state, cgroup throttle, query context | CPU time в окне. |
| Disk I/O | Backend logical/physical I/O evidence и wait | `read_bytes`/`write_bytes`, с отдельным PG buffer context. |
| Memory | RSS/PSS, shared/private, cgroup pressure | RSS/PSS и рост относительно baseline. |
| Replication | WAL sender/receiver, sync state, replay lag | Lag и длительность отклонения. |
| XID/Horizon | Old transactions, backend xmin, vacuum blockers | Возраст транзакции и влияние на horizon. |
| Sampling | Collector cadence, observed/missed risk, join coverage | Низкое покрытие и gaps. |

### 8.3. Композиция Overview

- Строка таблицы содержит PG backend context, узкий столбец точности связи и OS process metrics.
- Для OS-процесса используется идентичность `(boot_id, pid_namespace, pid, starttime)`, а не один PID. PostgreSQL `backend_start` и kernel `starttime` относятся к разным событиям и не сравниваются на точное равенство.
- Activity обозначается как point snapshot. Короткие запросы между циклами сбора могли не попасть в данные.
- Длительность heavyweight lock wait считается доказанной только при сохранённом `waitstart`. `query_start` и `xact_start` не называются временем удержания блокировки.
- Heatmap для activity отображает только наблюдавшиеся состояния и интервалы. Она не должна создавать иллюзию непрерывного исполнения между снимками.
- Переход по `queryid` открывает Statement Detail с атрибуцией `best_effort` в том же интервале; точная identity statement сейчас невозможна без `datid`, `usesysid` и семантики `toplevel`. Переход по PID открывает Process Detail с качеством relation; wait event — Waits & Locks.

## 9. Statements

### 9.1. Главный вопрос

Какие query fingerprints создали наибольший impact в окне и за счёт чего он изменился?

### 9.2. Разрезы

| Разрез | Смысловой центр | Основное ранжирование |
| --- | --- | --- |
| Workload | Вклад в total time, calls, rows | Доля total execution time. |
| Latency | Mean execution и planning time, Δ baseline | Рост mean time при значимом числе calls. |
| Buffers | Shared hits/read, rows, calls | Shared read blocks или падение hit ratio. |
| WAL | WAL bytes/records/FPI, calls | WAL bytes в окне. |
| Temp | Temp blocks/bytes, spill evidence | Temp bytes в окне. |
| Planning | Plan time share, calls, plan churn | Total planning time. |
| Regression | Изменение impact, calls, latency, buffers и plan marker | Взвешенная деградация к baseline. |
| Observed samples | Наблюдавшиеся activity/process samples для queryid | Число и покрытие наблюдений, не «точная CPU-стоимость запроса». |

### 9.3. Работа с 1 000 строк

- Default view показывает 18–21 строку и виртуализирует остальное.
- Ранжированная time matrix совмещает identity, 5–7 ключевых чисел и heatmap на 96 bucket.
- Sticky query identity и заголовок сохраняются при горизонтальном скролле.
- Group by database/user/application доступен как агрегация, но не меняет исходную сущность `queryid`.
- Строка раскрывается в Detail без потери фильтра, диапазона и baseline.

`pg_stat_statements` рассматривается как накопительный top-N snapshot. Дельты должны быть reset-aware. Текст query показывается только в detail с явным server-side cap; если PostgreSQL маскирует текст, интерфейс показывает `queryid` и `null`, а не выдуманный SQL.

Identity statement: `(queryid, userid, dbid, toplevel)` для `pg_stat_statements` 1.9+; для старых раскладок `toplevel` отсутствует. Ratio и per-call значения считаются из сумм валидных парных дельт, а не усреднением готовых interval ratios.

## 10. Plans

### 10.1. Главный вопрос

Менялся ли план для запроса, что именно изменилось в дереве и совпало ли это с деградацией?

### 10.2. Разрезы

| Разрез | Смысловой центр | Основное ранжирование |
| --- | --- | --- |
| Regression | Plan-change markers + statement impact before/after | Δ latency/impact вокруг смены плана. |
| Time | Plan mean/total time и calls | Total plan time. |
| I/O & Buffers | Shared hit/read и связанные relations | Shared reads или change to baseline. |
| Rows | Estimated/observed row evidence, если источник позволяет | Наибольшее расхождение или изменение. |
| Change timeline | Версии `planid` на общей оси | Частота смен и длительность плохого плана. |
| Compare | Side-by-side tree diff | Узлы с изменённым cost/rows/access path. |

### 10.3. Композиция Regression

- Верх: Health line и timeline смен `planid`.
- Середина: statement latency, calls, buffers и plan version lanes на одной оси.
- Низ: ranked queries with plan changes; справа — compact plan diff preview.
- Полноэкранный Plan Detail показывает дерево, changed nodes, связанные tables/indexes, окно before/after и provenance.

Поля и метод атрибуции по `queryid` зависят от варианта `pg_store_plans`. Для обоих вариантов связь со statement остаётся `best_effort` и не доказывает общую identity.

Для OSSC identity включает `(dbid, userid, queryid, planid)`, а атрибуция к statement использует `queryid`, `dbid` и `userid`. Для vadv identity включает `(dbid, userid, planid)`; `queryid_stat_statements` означает последний исполнивший запрос и не является надёжной per-query attribution. Gauge `mean_time` нельзя дифференцировать.

## 11. Tables

### 11.1. Главный вопрос

Какие отношения создавали нагрузку, накапливали churn или приближались к maintenance/freeze risk?

### 11.2. Разрезы

| Разрез | Смысловой центр | Основное ранжирование |
| --- | --- | --- |
| Pressure | Access + churn + maintenance + size | Composite risk с раскрываемой формулой. |
| Access | Seq/idx scans, tuples read/fetched | Tuples read и seq scan contribution. |
| Churn & HOT | Inserts/updates/deletes/HOT/dead tuples | Modified tuples и dead tuple growth. |
| Buffers | Heap/index/toast shared hit/read | Shared read blocks. |
| Maintenance | Last vacuum/analyze, modified since analyze | Maintenance backlog. |
| Freeze | XID/MXID age и пороги | Доля до freeze limit. |
| Size | Relation components, growth, time-to-full context | Total bytes и growth slope. |
| Related workload | Statements/plans that reference relation | Statement impact в выбранном окне. |

### 11.3. Table Detail

Смысловой центр: access mix → shared buffers → tuple churn → maintenance/freeze. Затем показываются индексы, запросы, планы, активные vacuum и связанные события.

`*_blks_hit` и `*_blks_read` относятся к shared buffers PostgreSQL. Shared-buffer miss не означает физический диск I/O. OS storage evidence показывается отдельной дорожкой и связывается только по времени, если нет более точного ключа.

Tables и Indexes могут собираться в разные моменты. Latest-as-of link показывает оба source timestamp и не подписывается как same-snapshot join. `n_dead_tup` — оценка, а не доказанный physical bloat.

## 12. Indexes

### 12.1. Главный вопрос

Используется ли индекс, окупает ли он стоимость хранения и записи, и какие планы от него зависят?

### 12.2. Разрезы

| Разрез | Смысловой центр | Основное ранжирование |
| --- | --- | --- |
| Usage | Scans, tuples read/fetched, last scan | Scans и recency. |
| Unused risk | Size, scans, write activity parent table | Потенциальная цена неиспользуемого индекса. |
| Size | Index size, table ratio, growth | Bytes и growth. |
| Buffers | `idx_blks_hit/read`, hit ratio | Shared read blocks. |
| Efficiency | Rows per scan, selectivity proxy, plan usage | Высокая цена на полезную строку. |
| Validity & build | Valid/ready/live flags и progress | Невалидные или незавершённые индексы. |
| Write amplification | Индексы таблицы × write churn | Оценка нагрузки на запись с явной формулой. |
| Plan dependencies | Plans and statements using index | Impact зависимых запросов. |

Удаление индекса никогда не предлагается как автоматический вывод только из `idx_scan = 0`. Detail показывает retention window, reset, размер, write churn, constraints, validity и найденные plan dependencies.

## 13. Vacuum

### 13.1. Главный вопрос

Что vacuum делает сейчас, где копится backlog и какие блокировки или ресурсы мешают обслуживанию?

### 13.2. Разрезы

| Разрез | Смысловой центр | Основное ранжирование |
| --- | --- | --- |
| Fleet | Active work + backlog + freeze risk | Composite maintenance urgency. |
| Active progress | Phase lanes, progress, elapsed | Elapsed и remaining blocks, если доступны. |
| Backlog | Dead tuples, modified since analyze, age | Backlog risk. |
| Phase | Vacuum phases across active workers | Длительность текущей фазы. |
| Freeze | XID/MXID age и horizon blockers | Freeze urgency. |
| Throughput | Processed blocks/tuples over observed history | Низкий throughput при достаточном покрытии. |
| Blockers | Locks, old transactions, backend xmin | Blocked duration и downstream impact. |
| Resource impact | Process CPU/I/O, cgroup throttle, device pressure | Resource usage during vacuum. |

ETA показывается только при достаточной истории и стабильной фазе. Иначе интерфейс показывает progress и throughput без прогноза. Active vacuum связывается с процессом по collector relation token или доказанному lifetime; locks, table, OS I/O и log events отображаются как отдельные evidence lanes.

Поля `num_dead_tuples` в PostgreSQL 10–16 и `num_dead_item_ids` в PostgreSQL 17+ не объединяются в одну колонку: это разные сущности и единицы. Stable run identity должна включать PID, ключ начала сессии или запроса, `datid` и `relid`.

## 14. Events

### 14.1. Главный вопрос

Какие дискретные события отмечают изменение системы и какие свидетельства нужно открыть рядом с ними?

### 14.2. Разрезы

| Разрез | Смысловой центр | Основное ранжирование |
| --- | --- | --- |
| Incident timeline | Typed events + health + gaps + selected incident | Severity, time и affected entities. |
| Errors | PostgreSQL errors and fatal events | Severity и повторяемость. |
| Checkpoints | Start/complete/duration/write/sync context | Duration и совпавший I/O pressure. |
| Vacuum & Analyze | Start/end/slow/skip-related events | Duration и affected relation. |
| Slow & Plans | Slow statements/auto_explain, если собраны | Duration и query/plan link quality. |
| WAL & Replication | Archive, receiver, sender, lag transitions | Длительность и severity. |
| Changes | Restart, config, deploy, failover markers | Время и scope. |
| Data quality | Collector gaps, unsupported, partial, corrupt | Потерянное покрытие и affected sources. |

Event Detail содержит исходное typed event, нормализованный summary, samples, связанные сущности и происхождение. Английские и русские шаблоны логов могут распознаваться, но `stderr` и поддерживаемые форматы должны быть указаны явно. Событие не превращается в root cause без отдельного доказательства.

## 15. Универсальный Entity Detail

Одинаковая анатомия применяется к process, activity, statement, plan, table, index, vacuum и event:

1. Entity header: человекочитаемое имя, typed identity, verdict, время, deep link.
2. Aligned evidence lanes: история сущности и связанные OS/PG/events на общей оси.
3. Entity-specific semantic center: схема, которая объясняет объект.
4. Metric matrices: current, baseline, delta, unit, coverage, verdict reason.
5. Related evidence: точные, временные и best-effort связи отдельными группами.
6. Provenance: источник, сборщик, cadence, reset, permissions, truncation и gaps.

Примеры смыслового центра:

- Process: PG shared buffers → OS logical I/O → приблизительная cache-served estimate → physical `read_bytes`.
- Activity: backend state/wait/query → PID identity → scheduler/I/O/memory evidence.
- Statement: calls × mean → total impact; buffers, WAL, temp, plans и observed samples.
- Plan: version timeline → changed tree nodes → related relations/indexes → statement before/after.
- Table: access mix → shared buffers → churn → maintenance/freeze.
- Index: usage → shared buffers → parent-table writes → size → plan dependencies.
- Vacuum: phase/progress → table backlog → blockers → process/device pressure.
- Event: typed message → affected interval → related entities → coincident signals.

## 16. Подсказки и мелкие взаимодействия

- Structured tooltip открывается через 200–250 мс и доступен по focus.
- Tooltip показывает definition, value, unit, window, aggregation, baseline, verdict rule, source и coverage.
- Щелчок закрепляет объяснение в Detail.
- `Shift+click` ставит baseline; chip рядом с диапазоном объясняет сравнение и позволяет его сбросить.
- Наведение на сущность подсвечивает её строку, heatmap, события и связанные lanes.
- `Enter` открывает Detail, `Esc` закрывает верхний слой, `/` открывает поиск.
- Null выводится как `—` с причиной; null не превращается в ноль.
- Gap остаётся штрихованным при hover и selection.
- Severity никогда не кодируется только цветом.

## 17. Подтверждённая семантика источников

- Activity — point snapshot; быстрые запросы между циклами могут быть пропущены.
- OS process snapshot требует `(boot_id, pid_namespace, pid, starttime)` из-за повторного использования PID и namespaces.
- Process detail может иметь `null` для `/proc/<pid>/io` из-за прав; это не ноль.
- `os_process` может быть ограничен cap и namespace; отсутствие процесса не доказывает отсутствие активности.
- Statements — накопительные top-N snapshots; текущая конфигурация может давать union примерно до 1 000 строк по нескольким осям.
- Counter series должны разрываться на restart/reset boundary.
- `pg_stat_io` агрегирован по `backend_type`, `object` и `context`, а не по PID или `queryid`.
- Нулевой timing может означать отключённый `track_io_timing`; невозможные комбинации дают `null`.
- Plans имеют разные возможности связи с statements в разных вариантах `pg_store_plans`.
- Typed log events имеют ограниченное число samples и не равны полному log viewer.
- Текущий фильтр выполняется на сервере, поддерживает до 16 terms, `field=value`, glob и типизированное равенство.
- `os_mountinfo` может быть last-known OnChange snapshot; UI показывает source age и не интерполирует изменения mount topology.
- Activity, vacuum, statements, tables, indexes и OS сейчас не имеют доказанного общего snapshot только потому, что их timestamp совпал. Для exact cross-source joins нужен `collection_cycle_id` или другой collector-produced snapshot token.
- Activity `query_id` относится к current query только при `state = active`; иначе это most recent query. Hash не считается стабильной identity между major versions и может коллидировать.
- Adaptive sampling 1 s начинается после trigger, а baseline cadence остаётся 5 s. Counts показываются как sampled/observed exposure с coverage, а не как ASH или точная длительность.

## 17.1. Правила вычислений

Counter delta допустима только для одной identity, положительного фактического интервала, без restart/reset/gap и при открытых feature gates:

```text
d(x) = x2 - x1
per_call(a) = sum(valid d(a)) / sum(paired valid d(calls))
hit_ratio = sum(d(hit)) / (sum(d(hit)) + sum(d(read)))
rate = sum(d(counter)) / sum(actual elapsed seconds)
```

Отрицательная дельта означает reset boundary, а не ноль. При нулевом знаменателе результат равен `null`. Gauges — connection count, `n_dead_tup`, progress, headroom, LSN gap и готовые mean values — не проходят через `diff`.

Restart по `postmaster_start_time` разрывает общие PG counters. Statements, plans, database/tables/indexes, WAL, archiver, checkpointer и `pg_stat_io` также используют собственные reset markers. `track_io_timing = false`, `track_wal_io_timing = false` и недоступный `track_planning` дают `NotCollected`, а не нулевую latency или planning cost.

Same timestamp не доказывает общий producer snapshot. PostgreSQL clock, collector OS clock и parsed log clock считаются разными clock domains, пока их связь не подтверждена.

## 18. Требования к API и модели данных

Дизайн требует или выигрывает от следующих контрактов:

- общий bucket grid и coverage по каждому источнику;
- cross-entity links с типом `exact | lifetime | temporal | best_effort | unavailable` и reason;
- collector-produced `collection_cycle_id`/snapshot token и host boot/PID namespace context;
- reset markers для всех накопительных серий;
- server-side глобальный поиск с group, match reason, continuation и scope;
- relationship endpoints для table/index/plan/statement/activity/process;
- prepared-view metadata: default sort, metric, columns, source groups и caveats;
- text summary для графика и heatmap для доступности;
- сохранение transient policy для свободного SQL-поиска;
- раздельные `severity`, `confidence` и `correlation`, без клиентского переопределения;
- 64-bit `queryid` и `planid` как decimal string, без преобразования в JavaScript `Number`.

## 19. Критерии приёмки макетов

- На 1920×1080 видны Global context, navigation, Health line, заголовок разреза, смысловой центр и не менее 16 строк evidence matrix.
- Каждый из восьми разделов имеет собственные подготовленные разрезы.
- На каждом экране присутствует хотя бы один явный переход к связанному типу сущности.
- Heatmap сохраняется в Activity, Statements и других mass-entity разрезах, где связь на глаз полезнее одного score.
- Все графики используют общий cursor и window.
- Activity не выглядит как непрерывный trace.
- PG shared-buffer reads не подписаны как physical disk reads.
- Statement CPU не показывается как точная атрибуция без соответствующего источника.
- Process page-cache estimate помечен как приблизительный.
- Связь plan→statement всегда помечена как `best_effort` и показывает provenance конкретного форка.
- Поиск показывает scope, match reason, число результатов и происхождение.
- `null`, gap, partial, gated, unsupported, reset и top-N truncation различимы.
- Здоровые состояния не создают зелёный визуальный шум.

## 20. Макеты для проверки

Для проверки системы достаточно одного репрезентативного экрана на раздел и двух сквозных экранов:

1. OS / Pressure.
2. Activity / Waits & Locks.
3. Statements / Buffers & WAL.
4. Plans / Regression.
5. Tables / Maintenance & Freeze.
6. Indexes / Usage & Risk.
7. Vacuum / Fleet & Progress.
8. Events / Incident Timeline.
9. Global Search.
10. Statement Detail или Index Detail как проверка универсальной анатомии.

Числа в макетах демонстрационные. Источники, ограничения, единицы и типы связей должны соответствовать реальной семантике продукта.

## 21. Проверенные Superdesign-макеты

Все макеты проверены в viewport 1920×1080. Предпочтительные ветки:

| Экран | Draft ID | Результат проверки |
| --- | --- | --- |
| OS / Pressure | `a444eb94-d25b-4cb2-9b9b-c978592e71da` | Общие OS/PG lanes, heatmap, evidence inspector и ranked resources помещаются над сгибом. |
| Activity / Waits & Locks | `57e405e9-7007-4536-83b0-7e7823991361` | Sampled waits, waiter-age Gantt, edge-only lock graph и 12 строк history видны одновременно. |
| Statements / Buffers | `745c03fe-93a3-47c9-87cd-799027c8e6c4` | 23 строки и статическая temporal heatmap видны без прокрутки; query text остаётся bounded detail-only полем. |
| Plans / Regression | `6b04f496-f1f3-47ba-81ec-ba3af3f8952a` | Plan mix, synchronized metrics, fork provenance и A/B diff помещаются над сгибом. |
| Tables / Maintenance & Freeze | `4167d669-3c15-4d23-b68b-b375008cc1fe` | Scatter, XID/MXID context, 15 relations и раздельные source timestamps видны одновременно. |
| Indexes / Usage & Risk | `a82ee76a-f697-462c-b8c8-02a59a62e9dc` | Review candidate, parent-table context и relation provenance заменяют автоматический вывод об удалении. |
| Vacuum / Fleet & Progress | `f7e4823f-0b72-472e-a05d-8bf4bef4367d` | Phase swimlanes, active runs и selected evidence совмещены на одном экране. |
| Events / Incident Timeline | `84ab1923-37d1-4516-a722-c04c2bc558ea` | Выбрана первая ветка: она оставляет event table над сгибом; временные совпадения остаются отдельными от causal links. |
| Global Forensic Search | `d89da887-a034-4ce9-9a14-ddf9d6e4631b` | Typed grammar, scope, grouped results, match reason и query-text availability показаны явно. |
| Statement Detail | `3822d51e-6a1d-44f0-b2db-0886da74b8eb` | Исправлены physical-I/O и causal claims; relation confidence отделён от временного совпадения. |

### 21.1. Ограничения демонстрационных данных

- Повторяющиеся или синтетические строки показывают плотность, а не реальную кардинальность текущего файла.
- В Vacuum Detail version-specific `num_dead_tuples`, `num_dead_item_ids` и bytes должны показываться условно по версии источника; одновременное заполнение недопустимо.
- В Index Detail constraint flags должны приходить из каталога. Метка primary/unique никогда не вычисляется из имени индекса и не означает рекомендацию удалить или сохранить объект.
- Correlation score в OS-макете — только необязательный способ ранжирования совпадений. Текстовый вывод и тип линии остаются главным обозначением недоказанной причинности.
- Plan table может содержать мало строк, если в выбранном окне реально наблюдалось только несколько plan identities. Пустое пространство заполняется evidence detail, но не синтетическими «активными планами» в production UI.
- Event timeline использует фиксированную высоту. При большем числе категорий таблица получает минимум 8–10 строк над сгибом за счёт сворачивания спокойных lanes.

## 22. Решения после экспертного аудита

Консультации Linux/DevOps, DBA и PostgreSQL development привели к следующим решениям:

- отказаться от плоской навигации по сырым источникам;
- оставить верхнюю группировку без постоянного левого sidebar, чтобы не отнимать ширину у heatmap и SQL identity;
- сделать OS Pressure реализацией USE method, но не смешивать ресурсы на одной числовой шкале;
- считать locks отдельным snapshot graph, а не обычным join к Activity;
- разделить waiter age, blocker transaction age и неизвестное lock hold time;
- разделить OSSC и vadv plan identities и attribution;
- не использовать exact cross-source join без collector token или доказанного lifetime;
- разрывать counter series на reset, gap, top-N absence и превышении max gap;
- считать statements ratios только из сумм валидных paired deltas;
- показывать tables/indexes latest-as-of timestamps отдельно;
- условно отображать version-specific Vacuum units;
- считать log source status полосой качества, а не событием;
- отделить search fields от display columns и хранить 64-bit identifiers как строки.
