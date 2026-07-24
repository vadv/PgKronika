# PgKronika demo-стенд — спека v1 (оценка размеров)

Версия 0.1, 2026-07-24. Черновик на ревью.

Цель этой итерации — один: получить **реальные размеры** сегмента PGM и факт-файла `.ovf` под представительной нагрузкой, чтобы решить, нести ли charts в индекс или считать их on-demand из PGM. Web-UI, MCP, визуализация — следующая итерация, здесь вне scope.

Образец — demo-стенд reftool (`reftool demo/` + крейт `reftool-demo`): один самодостаточный Docker-контейнер с PostgreSQL под синтетической нагрузкой. Повторяем его подход, только вместо reftool собирает `pg_kronika-collector`, и добавляем шаг построения `.ovf` + замер.

## 1. Что уже есть и переиспользуется

- `flake.nix` / `Dockerfile.bdd-builder` — сборка PostgreSQL 15–18 с `pg_stat_statements` и `pg_store_plans`. База для контейнера стенда, не собирать PG заново.
- `pg_kronika-collector` — пишет сегменты. Конфигурация через env: `KRONIKA_PG_DSN`, `KRONIKA_OUT_DIR` (каталог сегментов), `KRONIKA_SOURCE_ID`, `KRONIKA_SEGMENT_MAX_AGE_S` (900 = 15-мин сегменты), лимиты `KRONIKA_PG_MAX_TABLES/INDEXES/STATEMENTS/PLANS` (по умолчанию 500).
- overview index-build — `SegmentFacts::extract` из `kronika-reader`; сборка `.ovf` для sealed-сегмента уже работает (M2).

Важный факт для оценки: collector собирает **top-500** таблиц/индексов/statements/plans. Значит размер PGM имеет потолок и не растёт бесконечно с размером БД — он определяется этими лимитами плюс числом бэкендов и OS-метриками. И PGM, и `.ovf` — величины ограниченные и предсказуемые; демо-стенд нужен, чтобы измерить их на насыщенном профиле (упереться в лимиты).

## 2. Компоненты стенда

### 2.1 PostgreSQL-окружение (копия подхода reftool)

Контейнер: PG (сначала одна версия, 17) + `pg_stat_statements` + `pg_store_plans` + два tablespace. `postgresql.conf` по образцу reftool, с наблюдаемостью для событийных блоков:

```
max_connections = 100
shared_buffers = 128MB
shared_preload_libraries = 'pg_store_plans,pg_stat_statements'
pg_stat_statements.track = all
pg_store_plans.track = all
logging_collector = on
log_min_duration_statement = 1000
log_checkpoints = on
log_lock_waits = on
log_autovacuum_min_duration = 0
deadlock_timeout = 1s
```

cgroup v2 доступен в контейнере (для OS-метрик collector'а).

### 2.2 Генератор нагрузки

Крейт `bins/pg_kronika-demo` (аналог `reftool-demo`; допустимо прямо переиспользовать схему reftool-demo). Задача — **насытить** профиль до лимитов collector, чтобы замер был по верхней планке.

Схема (реалистичный OLTP + фон):
- `accounts` (~10 000 строк, seed батчами), `orders`, `locked_resource` — OLTP-ядро.
- `staging.large_scan` (~200 MB, 500k строк) — seq-scan и вытеснение буфера.
- `audit.logs` / `audit.events` — вставочная нагрузка.
- Параметр `DEMO_TABLES` — доп. пустые/мелкие таблицы + индексы, чтобы дойти до сотен объектов (упереться в top-500).

Сценарии (потоки, как в reftool-demo):
- OLTP: INSERT/UPDATE — TPS, tuple writes, buffer hits.
- Seq-scan по `large_scan` — buffer eviction, disk read.
- Lock-contention на `locked_resource` — lock waits, изредка deadlock.
- Фон: autovacuum, checkpoints (из конфига), редкие ошибки (нарушение constraint, statement timeout) — для событийных блоков `.ovf`.
- Опционально crash-recovery (флаг), как в reftool.

Параметры (env, чтобы гонять разные профили):
- `DEMO_BACKENDS` — число активных соединений (20–50).
- `DEMO_TPS` — интенсивность OLTP.
- `DEMO_TABLES` / `DEMO_INDEXES` — насыщение объектами (до 300 / 500).
- `DEMO_DURATION_MIN` — длительность (30–60 → 2–4 сегмента).

### 2.3 Collector

Запускается в том же контейнере против demo-БД: `KRONIKA_PG_DSN=...`, `KRONIKA_OUT_DIR=/data/segments`, `KRONIKA_SEGMENT_MAX_AGE_S=900`, лимиты по умолчанию (500). Пишет реальные `.pgm` под нагрузкой.

### 2.4 Index-build + замер

После прогона (когда накопилось ≥2 запечатанных сегмента):
1. Для каждого `.pgm` построить `.ovf` (reader-утилита / dump-режим).
2. Скрипт `scripts/measure.sh`:
   - размер каждого `.pgm` и суммарный;
   - размер каждого `.ovf` и суммарный;
   - отношение `.ovf : .pgm` (среднее и на сегмент);
   - разбивка `.ovf` по блокам (из каталога: сколько байт `EventObservations`, `GaugeSamples`, `CounterSamples`, `LossCoverage`, `StringTable`, …) — увидеть, что доминирует;
   - экстраполяция charts: `+N_серий × snapshots × 24 байта` (сырьё сэмпла) под ZSTD → прирост `.ovf` → пересчёт отношения при 19 добавленных сериях.

## 3. Что измеряем (выход итерации)

| Величина | Зачем |
| --- | --- |
| Размер `.pgm` / сегмент под нагрузкой | реальный потолок сырья (top-500) |
| Размер `.ovf` / сегмент (текущий: события + coverage + факторные сэмплы health) | реальный размер индекса как есть |
| Отношение `.ovf : .pgm` | текущая экономия индекса |
| Разбивка `.ovf` по 9 блокам | какой блок доминирует, где растёт при charts |
| Экстраполяция при +19 chart-серий | размер `.ovf` с charts → решение A/B |

## 4. Порог решения (charts в индекс или нет)

- Если с charts `.ovf` остаётся **много меньше** `.pgm` (условно < ~10–15%) — charts в индекс (путь A) оправданы: дёшево и по CPU (кэш), и по диску.
- Если charts раздувают `.ovf` до сопоставимого с `.pgm` — путь A теряет смысл: дешевле считать charts **on-demand** из PGM (путь B — перечитать sealed-сегмент за диапазон + стримить хвост active parts), не храня их в индексе.
- Конкретную границу назначаем по замеру, не заранее.

## 5. Границы этой итерации

Только размеры. Вне scope (следующие итерации):
- Web-UI и оценка визуализации.
- MCP, `/analysis`, `/ai-prompt`.
- Сама реализация charts (ChartPolicy, ручка `/v1/timeline/charts`).
- Бенчмарки латентности (это M6 §18; стенд даёт для них dense-hour фикстуру, но замер латентности — отдельно).

## 6. Реализация

Новый крейт `bins/pg_kronika-demo` + каталог `demo/` (Dockerfile, entrypoint.sh, postgresql.conf, self-check.sh) + `scripts/measure.sh`. Не трогает overview-код в `main` (crates/kronika-reader, bins/pg_kronika-web) — можно делать параллельно ревью открытых PR. Первый прогон — одна версия PG (17), один профиль нагрузки; расширение версий/профилей — по потребности.
