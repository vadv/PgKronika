# Дизайн: `pg_stat_user_tables` (type `1_013`)

- **Дата:** 2026-06-29
- **Статус:** на ревью (brainstorming → spec)
- **type_id:** `1_013_001` (PG10-12) / `1_013_002` (PG13-15) / `1_013_003` (PG16-18)
- **Класс метрики:** B (database-local) — первая в проекте
- **Поправка 2026-06-30:** danger-ветка отбора (формулы autovacuum + `KRONIKA_PG_WRAPAROUND_WARN_FRACTION`, §3/§5/§12) УДАЛЕНА — это аналитика, а коллектор её не делает (принцип «регистратор, не аналитик»). Отбор кандидатов теперь чисто механический: top-N по сырым колонкам, включая `age(relfrozenxid)` и `mxid_age(relminmxid)`. Сигналы остаются колонками; пороги применяет отдельный модуль анализа.
- **Поправка 2026-07-01 (по ревью PR #30):** добавлена версия `1_013_004` (PG18: +`total_vacuum_time`/`total_autovacuum_time`/`total_analyze_time`/`total_autoanalyze_time`; V3 стала PG16-17); `size_bytes` → `main_fork_bytes` (это main fork, не total); добавлена механическая ось отбора по записи (`n_tup_ins+upd+del`). Тело ниже (§3-§5, счётчики колонок) — исторический снимок; **актуальная схема — `docs/type-registry/postgresql.md`**.

## 1. Цель и контекст

Первая database-local метрика и первый потребитель пула соединений (`ConnectionPool`,
PR #29). Снимает по каждой подключаемой базе инстанса статистику таблиц:
доступ (seq/idx), запись, vacuum/analyze, bloat, размеры, буферный I/O, а также
сигналы приближения к wraparound и просрочки autovacuum.

Зачем отдельный класс сбора: instance-wide метрики (class A) читают `pg_stat_*`
один раз через `pool.main()`. Статистика таблиц живёт **в каждой базе отдельно** —
прочитать её можно только подключившись к каждой базе. Пул это уже умеет
(`per_db()`), но демон ещё не вызывает `refresh()` и не итерирует базы — это
делает данный эпик.

## 2. Scope

В объёме:
- Новый type_id `1_013` с тремя версиями схемы.
- Модуль сбора `kronika-source-pg/src/user_tables.rs`.
- Включение per-db сбора в демон: вызов `pool.refresh()`, итерация `per_db()`.
- Применение `AdaptiveTimeout` к тяжёлому запросу (впервые в проекте).
- BDD-сценарий на мульти-базовое разделение и golden-кодеки PG10-13.

Вне объёма (отдельные эпики):
- `1_004` `pg_stat_user_indexes` (id зарезервирован).
- Покрытие баз отдельной секцией сегмента (тип coverage).
- Single-db режим (`PGDATABASE`).
- source_id default = host:port (option B) — отдельный мини-цикл формата.
- Per-table reloptions для точных порогов autovacuum (см. §10).
- Параллельный сбор по базам (см. §10).

## 3. Архитектура: отбор кандидатов

При большом числе таблиц снимать все нельзя. rpglot берёт top-N по трём осям
объёма. Этого мало: мелкая, бездействующая, не раздутая таблица может быть
критичной по wraparound — и в top-N по объёму не попадёт никогда.

Решение — две независимые стратегии отбора в одном `WITH`:

1. **Объём → top-N** (floor rpglot): активность ∪ размер ∪ bloat, каждая ось
   `LIMIT N` (env `KRONIKA_PG_MAX_TABLES`, дефолт 500). Итог ≤ 3N уникальных.
2. **Опасность → порог** (above-floor): включаем **все** таблицы, которые
   пересекли линию опасности — по собственной логике autovacuum PostgreSQL
   плюс wraparound. В здоровой базе ветка возвращает 0 строк; при угрозе — все
   опасные, независимо от N.

Пороговая ветка повторяет решающие правила `autovacuum.c`, а не произвольные
константы:

| Сигнал | Условие | Версии |
|---|---|---|
| A. xid wraparound | `age(relfrozenxid) > afma * 0.8` | все |
| B. multixact wraparound | `mxid_age(relminmxid) > amfma * 0.8` | все |
| C. vacuum просрочен | `n_dead_tup > vac_t + vac_sf * reltuples` | все |
| D. insert-freeze долг | `n_ins_since_vacuum > ins_t + ins_sf * reltuples` | PG13+ |
| E. analyze просрочен | `n_mod_since_analyze > ana_t + ana_sf * reltuples` | все |

`afma`/`amfma`/пороги/scale-факторы берутся из `current_setting()` (реальные GUC
инстанса, не хардкод). Множитель `0.8` для wraparound — env-tunable
(`KRONIKA_PG_WRAPAROUND_WARN_FRACTION`): `0.5` шумит (активные таблицы routinely
доходят до половины `afma` между freeze), `0.8` ловит отстающих с запасом до
форс-vacuum при `afma`.

## 4. Полная схема (V3, надмножество)

Классы: **L** label · **T** ts · **C** counter (rate) · **G** gauge (snapshot value).
Корректность класса критична: counter и gauge по-разному обрабатываются на чтении.

| Колонка | Тип | Класс | Null | Источник | Версии |
|---|---|---|---|---|---|
| `relid` | u32 | L | нет | `pg_stat_user_tables.relid` | все |
| `datname` | StrId | L | нет | имя базы соединения | все |
| `schemaname` | StrId | L | нет | `schemaname` | все |
| `relname` | StrId | L | нет | `relname` | все |
| `tablespace` | StrId | L | нет | `pg_tablespace.spcname`, иначе `pg_default` | все |
| `ts` | Ts | T | нет | время сбора | все |
| `seq_scan` | i64 | C | нет | | все |
| `seq_tup_read` | i64 | C | нет | | все |
| `idx_scan` | i64 | C | **да** (NULL = нет индексов) | | все |
| `idx_tup_fetch` | i64 | C | **да** (NULL = нет индексов) | | все |
| `n_tup_ins` | i64 | C | нет | | все |
| `n_tup_upd` | i64 | C | нет | | все |
| `n_tup_del` | i64 | C | нет | | все |
| `n_tup_hot_upd` | i64 | C | нет | | все |
| `n_tup_newpage_upd` | i64 | C | нет | | **V3** |
| `n_live_tup` | i64 | G | нет | | все |
| `n_dead_tup` | i64 | G | нет | | все |
| `n_mod_since_analyze` | i64 | G | нет | | все |
| `n_ins_since_vacuum` | i64 | G | нет | | **V2+** |
| `vacuum_count` | i64 | C | нет | | все |
| `autovacuum_count` | i64 | C | нет | | все |
| `analyze_count` | i64 | C | нет | | все |
| `autoanalyze_count` | i64 | C | нет | | все |
| `last_vacuum` | i64 | G | **да** (NULL = никогда) | epoch sec | все |
| `last_autovacuum` | i64 | G | **да** | epoch sec | все |
| `last_analyze` | i64 | G | **да** | epoch sec | все |
| `last_autoanalyze` | i64 | G | **да** | epoch sec | все |
| `last_seq_scan` | i64 | G | **да** | epoch sec | **V3** |
| `last_idx_scan` | i64 | G | **да** | epoch sec | **V3** |
| `size_bytes` | i64 | G | нет | `pg_relation_size(relid)` | все |
| `toast_bytes` | i64 | G | **да** (NULL = нет TOAST) | `pg_total_relation_size(reltoastrelid)` | все |
| `toast_n_live_tup` | i64 | G | **да** | `pg_stat_get_live_tuples(reltoastrelid)` | все |
| `toast_n_dead_tup` | i64 | G | **да** | `pg_stat_get_dead_tuples(reltoastrelid)` | все |
| `toast_last_autovacuum` | i64 | G | **да** | epoch sec | все |
| `xid_age` | i64 | G | нет | `age(relfrozenxid)` | все |
| `mxid_age` | i64 | G | нет | `mxid_age(relminmxid)` | все |
| `reltuples` | i64 | G | нет | `pg_class.reltuples` (-1 = не анализирована, PG14+) | все |
| `heap_blks_read` | i64 | C | нет | `pg_statio_user_tables` | все |
| `heap_blks_hit` | i64 | C | нет | | все |
| `idx_blks_read` | i64 | C | **да** (NULL = нет индексов) | | все |
| `idx_blks_hit` | i64 | C | **да** | | все |
| `toast_blks_read` | i64 | C | **да** (NULL = нет TOAST) | | все |
| `toast_blks_hit` | i64 | C | **да** | | все |
| `tidx_blks_read` | i64 | C | **да** | | все |
| `tidx_blks_hit` | i64 | C | **да** | | все |

`sort_key("ts", "datname", "relid")` — детерминированный порядок строк.

Дельты версий (монотонные add'ы → отдельные type_id, дисциплина реестра):
- **V1** (`1_013_001`, PG10-12): базовый набор без `n_ins_since_vacuum`,
  `n_tup_newpage_upd`, `last_seq_scan`, `last_idx_scan` — 41 колонка.
- **V2** (`1_013_002`, PG13-15): + `n_ins_since_vacuum` — 42 колонки.
- **V3** (`1_013_003`, PG16-18): + `n_tup_newpage_upd`, `last_seq_scan`,
  `last_idx_scan` — 45 колонок.

NULL вместо 0 для «никогда»/«неприменимо» (last_*, toast_* без TOAST, idx/tidx
без индексов) — улучшение над rpglot, который всё COALESCE'ит в 0 и теряет
различие «нет индексов» против «индексы есть, но не сканировались».

## 5. SQL по версиям

V3 (PG16-18), остальные — те же ветки минус новинки и минус insert-терм (D) на V1:

```sql
WITH s AS (
  SELECT current_setting('autovacuum_freeze_max_age')::int8            AS afma,
         current_setting('autovacuum_multixact_freeze_max_age')::int8  AS amfma,
         current_setting('autovacuum_vacuum_threshold')::int8          AS vac_t,
         current_setting('autovacuum_vacuum_scale_factor')::float8     AS vac_sf,
         current_setting('autovacuum_vacuum_insert_threshold')::int8   AS ins_t,   -- PG13+
         current_setting('autovacuum_vacuum_insert_scale_factor')::float8 AS ins_sf, -- PG13+
         current_setting('autovacuum_analyze_threshold')::int8         AS ana_t,
         current_setting('autovacuum_analyze_scale_factor')::float8    AS ana_sf
),
candidates AS (
  (SELECT relid FROM pg_stat_user_tables
     ORDER BY GREATEST(last_seq_scan, last_idx_scan) DESC NULLS LAST LIMIT $1)        -- активность (V1/V2: сумма счётчиков)
  UNION
  (SELECT t.relid FROM pg_stat_user_tables t JOIN pg_class c ON c.oid = t.relid
     ORDER BY c.relpages DESC LIMIT $1)                                              -- размер
  UNION
  (SELECT relid FROM pg_stat_user_tables ORDER BY COALESCE(n_dead_tup,0) DESC LIMIT $1)  -- bloat
  UNION
  (SELECT t.relid FROM pg_stat_user_tables t
     JOIN pg_class c ON c.oid = t.relid CROSS JOIN s
     WHERE age(c.relfrozenxid)    > s.afma  * $2                       -- A
        OR mxid_age(c.relminmxid) > s.amfma * $2                       -- B
        OR t.n_dead_tup           > s.vac_t + s.vac_sf  * c.reltuples  -- C
        OR t.n_ins_since_vacuum   > s.ins_t + s.ins_sf  * c.reltuples  -- D (PG13+)
        OR t.n_mod_since_analyze  > s.ana_t + s.ana_sf  * c.reltuples) -- E
)
SELECT t.relid, t.schemaname, t.relname, COALESCE(ts.spcname,'pg_default') AS tablespace,
       t.seq_scan, t.seq_tup_read, t.idx_scan, t.idx_tup_fetch,
       t.n_tup_ins, t.n_tup_upd, t.n_tup_del, t.n_tup_hot_upd, t.n_tup_newpage_upd,
       t.n_live_tup, t.n_dead_tup, t.n_mod_since_analyze, t.n_ins_since_vacuum,
       t.vacuum_count, t.autovacuum_count, t.analyze_count, t.autoanalyze_count,
       EXTRACT(EPOCH FROM t.last_vacuum)::int8      AS last_vacuum,
       EXTRACT(EPOCH FROM t.last_autovacuum)::int8  AS last_autovacuum,
       EXTRACT(EPOCH FROM t.last_analyze)::int8     AS last_analyze,
       EXTRACT(EPOCH FROM t.last_autoanalyze)::int8 AS last_autoanalyze,
       EXTRACT(EPOCH FROM t.last_seq_scan)::int8    AS last_seq_scan,
       EXTRACT(EPOCH FROM t.last_idx_scan)::int8    AS last_idx_scan,
       pg_relation_size(t.relid)::int8 AS size_bytes,
       CASE WHEN cl.reltoastrelid <> 0 THEN pg_total_relation_size(cl.reltoastrelid)::int8 END AS toast_bytes,
       CASE WHEN cl.reltoastrelid <> 0 THEN pg_stat_get_live_tuples(cl.reltoastrelid) END AS toast_n_live_tup,
       CASE WHEN cl.reltoastrelid <> 0 THEN pg_stat_get_dead_tuples(cl.reltoastrelid) END AS toast_n_dead_tup,
       CASE WHEN cl.reltoastrelid <> 0 THEN EXTRACT(EPOCH FROM pg_stat_get_last_autovacuum_time(cl.reltoastrelid))::int8 END AS toast_last_autovacuum,
       age(cl.relfrozenxid)::int8 AS xid_age, mxid_age(cl.relminmxid)::int8 AS mxid_age, cl.reltuples::int8 AS reltuples,
       io.heap_blks_read, io.heap_blks_hit, io.idx_blks_read, io.idx_blks_hit,
       io.toast_blks_read, io.toast_blks_hit, io.tidx_blks_read, io.tidx_blks_hit
FROM pg_stat_user_tables t
JOIN candidates cand ON cand.relid = t.relid
LEFT JOIN pg_class cl ON cl.oid = t.relid
LEFT JOIN pg_tablespace ts ON ts.oid = cl.reltablespace
LEFT JOIN pg_statio_user_tables io ON io.relid = t.relid;
```

Параметры: `$1` = N (top-N), `$2` = wraparound-fraction.
Версийные ветки (`const fn user_tables_query(version)`):
- V1: ось активности → сумма счётчиков; убраны `n_tup_newpage_upd`,
  `last_seq_scan`, `last_idx_scan`, весь insert-терм D и его GUC из `s`.
- V2: ось активности → сумма счётчиков; убраны три V3-колонки.

statio слит в тот же запрос через `LEFT JOIN pg_statio_user_tables` (атомарный
снимок; rpglot делает двумя запросами с merge, его отдельный statio-top-N
эффективно отбрасывается — LEFT JOIN на тот же candidate-set ≥ паритет).

## 6. Сбор и интеграция в демон

Модуль `kronika-source-pg/src/user_tables.rs` (паттерн как `database.rs`):
`enum UserTablesVersion {V1,V2,V3}`, `user_tables_version(major)`,
`user_tables_query(version)` (в `marked!`), `struct UserTablesRow` (owned),
`to_v1/to_v2/to_v3<E>(row, datname, intern)` (чистые, golden-тестируемые),
`async collect_user_tables(client, major, max_tables, wrap_fraction)`.

Демон (`bins/pg_kronika-collector/src/main.rs`, `snapshot_and_seal`):
1. после `ensure_main()` — `pool.refresh(refresh_interval, max_databases)`;
   залогировать `pool.uncovered()` (базы без живого соединения).
2. сбор class-A через `pool.main()` (как сейчас).
3. сбор class-B: цикл по `pool.per_db()` (**последовательно**, см. §10),
   с каждого — `collect_user_tables(db.client(), major, …)`, `datname` из
   `db.datname`.

## 7. Модель памяти (интернинг)

**Collect-all-then-intern** (playbook-паттерн): сначала все `collect_*().await`
по всем базам, накапливая owned-строки; затем строится `Interner`/`SectionBuffers`
и идёт интернинг без await.

Обоснование (ревизия записанного контракта «инкрементально, пик = одна база»):
runtime `rt-multi-thread`, но `snapshot_and_seal` awaited напрямую (не spawned),
поэтому `block_on` не требует Send — компилируются обе модели. Top-N ограничивает
сумму до ~30k строк (≤20 баз × ≤1500), сырьё ≈ единицы МБ. Контракт писался под
неограниченный сбор; top-N его обнуляет. Send-сырьё оставляет дверь под будущий
bounded-parallel сбор. Деталь реализации — формат сегмента идентичен в обеих
моделях.

Связь с codec-cap: секция типа `1_013` объединяет строки всех баз; жёсткий
предел кодека — 65536 строк. При дефолте `N=500` × `max_databases=20` ≈ 30k —
безопасно. При повышении обоих параметров приблизимся к пределу → log + усечение
с маркером.

## 8. Таймауты и обработка ошибок

Тяжёлый запрос (`pg_relation_size`/`pg_total_relation_size` на ≤3N таблиц) —
первый случай применения `AdaptiveTimeout` (один на инстанс, старт 15s ×2 до 60s):
- `SET statement_timeout` перед запросом из текущего значения адаптива;
- SQLSTATE `57014` (statement_timeout) → `grow()` и повтор той же базы;
- SQLSTATE `55P03` (lock_timeout) → пропуск базы без роста (чужая блокировка,
  не стоимость запроса);
- прочие ошибки базы → лог, пропуск базы, остальные базы собираются.

Падение одной базы не срывает сегмент: собираем что смогли, помечаем пропуски.

## 9. Тестирование

- BDD `features/user_tables.feature`: матрица создаёт ≥2 базы с таблицами →
  проверяем строки из **обеих** баз (datname-разделение — суть class B), декод
  `1_013_00x`, резолв StrId через словарь сегмента. Отдельный сценарий: таблица
  с искусственно состаренным `relfrozenxid` попадает в выборку при пустой
  активности (проверка danger-ветки A).
- Golden-кодеки для PG10-13 (нет в live-матрице nixpkgs).
- Unit: `to_vN` (NULL-маппинг toast/idx без TOAST/индексов), `user_tables_version`
  на границах 13/16, парсинг `reltuples = -1`.

## 10. Оговорки и будущее

1. **Per-table reloptions** (таблица со своим `autovacuum_vacuum_scale_factor`
   или `autovacuum_freeze_max_age`) — v1 использует GUC; для override'нутых
   таблиц порог danger-ветки чуть неточен. Refine:
   `pg_options_to_table(c.reloptions)` COALESCE с GUC.
2. **Параллельный сбор по базам** — v1 последовательный (мягче к хосту БД: нет
   20 одновременных `pg_relation_size`-штормов; адаптивный таймаут с одним
   запросом проще). bounded-parallel — будущая оптимизация.
3. **`candidate_reason`** (битмаска причины попадания) — отложена; консьюмер
   выводит причину из значений колонок.
4. `reltuples` хранится как есть (`-1` = не анализирована в PG14+); формула
   danger использует сырое float-значение до приведения.

## 11. Пять точек правок (playbook)

1. `docs/type-registry/postgresql.md` — строки сводной таблицы + секция типа
   `1_013` с тремя версиями.
2. `crates/kronika-registry/src/codec/pg_stat_user_tables.rs` — три
   `#[derive(Section)]` структуры; подключить в `codec.rs` и `lib.rs`
   (`pub use` + `registry()`).
3. `crates/kronika-source-pg/src/user_tables.rs` + `mod`/`pub use` в `lib.rs`.
4. `bins/pg_kronika-collector/src/main.rs` — `refresh()`, цикл `per_db()`,
   `collect_user_tables`, буферизация через `match version`.
5. `crates/kronika-bdd/` — `features/user_tables.feature` + then-шаг.

## 12. Env-переменные

| Переменная | Дефолт | Назначение |
|---|---|---|
| `KRONIKA_PG_MAX_TABLES` | 500 | N для каждой оси top-N |
| `KRONIKA_PG_WRAPAROUND_WARN_FRACTION` | 0.8 | доля `afma`/`amfma` для danger A/B |
| `KRONIKA_PG_POOL_REFRESH_SECS` | 600 | мин. интервал `pool.refresh()` |
| `KRONIKA_PG_HEAVY_TIMEOUT_CAP_MS` | 60000 | потолок адаптивного таймаута |

## 13. Открытые решения (для gate-ревью спека)

Подтверждены пользователем (parity-plus + «продолжить»):
- ① danger = все 5 (A-E); ② порог wraparound `0.8*afma`; ③ danger + 3 колонки
  в v1; ④ `candidate_reason` отложен; type_id `1_013`; statio слит; NULL-вместо-0.

**Требует подтверждения на ревью:**
- **Модель интернинга** (§7): ревизия записанного контракта «инкрементально,
  пик = одна база» → collect-all-then-intern. Деталь реализации, формат идентичен,
  легко флипнуть.
- **Размещение спека** в `docs/superpowers/specs/` (skill default) против
  `docs/` (где лежит `connection-and-multidb.md`).
