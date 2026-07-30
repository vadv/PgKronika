# Полнота сбора секций: дешёвый source_total и статус «добрано N из M»

Дата: 2026-07-30. Базируется на main после #138. Цель — показывать по
каждой высококардинальной секции, сколько строк реально добрано и сколько
всего было в источнике, не только бинарный факт обрезки, и получать это
без дорогих блокировок расширений.

## Задача

Секции statements, plans, user_tables, user_indexes собираются top-N —
добирается не всё. Интерфейсу нужен статус вкладки «добрано N из M»
(и почему обрезано), как в референсном инструменте. Сейчас показывается
только косвенный признак обрезки, а точное M местами берётся ценой
чтения текстов запросов/планов под блокировкой.

## Что уже есть (не изобретать)

Инфраструктура полноты per-section в основном на месте:

- `SnapshotCoverageV1` (registry, type_id 1_038_001) на КАЖДУЮ секцию
  несёт `section_type_id`, `collected` («rows durably written»),
  `source_total` («rows observed before collector-side withholding»),
  `read_state` (`0=complete`, `1=source_limit`, `2=permission`,
  `3=read_failure`, `4=collector_limit_or_loss`), `visibility`.
- Коллектор строит его через `coverage::snapshot_coverage(...)`;
  накопитель `SourceCoverage{ total, collected, unknown_total, … }`
  уже имеет флаг `unknown_total` — «total не точный».
- Reader разбирает 1_038_001 в `SourcePopulation{ collected, total,
  total_quality: Exact|LowerBound }` с `lost_count_lower_bound`.
- Web уже отдаёт `collected` в `/v1/timeline/health`
  (`health.rs:150`).

То есть модель «добрано collected из source_total, точность total,
причина обрезки» существует. Проблема не в модели, а в двух местах:
цена получения `source_total` и неполный доступ к тройке из UI.

## Проблема 1 — source_total читает тексты под блокировкой

`pg_stat_statements` и `pg_store_plans` хранят тексты запросов/планов во
внешнем файле; чтение текста берёт блокировку и делает I/O. Аргумент
`showtext=false` пропускает файл и проходит только хеш в разделяемой
памяти. Текущий сбор `source_total` этого не соблюдает:

- statements (`statements.rs:136`): `count(*) FROM pg_stat_statements` —
  без `(false)`, дефолт `showtext=true` → count тянет весь файл текстов.
- plans, ossc-форк (`store_plans.rs:540,589`): `count(*) FROM
  pg_store_plans` — у ossc нет аргумента `showtext`, count материализует
  тексты планов.
- plans, vadv-форк (`store_plans.rs:156`): `pg_store_plans(false)` —
  корректно, без текстов.
- Плюс `count(*)` — второй проход set-returning-функции помимо
  candidate-запроса: два обхода хеша расширения на каждый сбор.

На большом хеше расширения это удержание блокировки и лишний I/O — то,
чего сбор обязан избегать.

## Ключевой принцип: «добрано» не требует точного M

`collected` известен бесплатно — это размер записанного набора. Факт
«добрали не всё» тоже берётся бесплатно приёмом «candidate `LIMIT N`,
fetch `N+1`»: если вернулось больше N — обрезано (`read_state=1`,
`source_total` — нижняя граница), если не больше — добрано всё
(`source_total = collected`, `Exact`). Так делает activity
(`ACTIVITY_FETCH_ROWS = MAX_ACTIVITY_ROWS + 1`).

Точное M — отдельная роскошь, оправданная только там, где счёт дёшев.
Классификация источников по цене точного `source_total`:

| Источник | Дешёвый точный count | Как получать source_total |
| --- | --- | --- |
| statements | да — `pg_stat_statements(false)` | точный, `showtext=false` |
| plans (vadv) | да — `pg_store_plans(false)` | точный, `showtext=false` |
| plans (ossc) | нет — нет `showtext=false`, count тянет тексты | LowerBound через `N+1`, `unknown_total=true` |
| user_tables | да — обычная view над статистикой, без внешних файлов | точный `count(*)` |
| user_indexes | да — то же | точный `count(*)` |

## Решение

### Сбор

1. **Ни один запрос `source_total` не читает тексты.** statements →
   `pg_stat_statements(false)`; plans-ossc → убрать `count(*) FROM
   pg_store_plans`; plans-vadv остаётся `pg_store_plans(false)`.
2. **Где дешёвого точного count нет (ossc), `source_total` не считается
   отдельным проходом.** Candidate добирает `top-N`, но fetch `N+1`:
   больше N → `read_state=1` (source_limit), `unknown_total=true`,
   `source_total` — наблюдаемая нижняя граница (`collected + 1`: видели
   лишнюю строку), а точное M не платится. Флаг `source_limit` несёт
   «есть ещё» независимо от числа.
3. **Где count дёшев (statements(false), vadv plans(false),
   tables/indexes), точный `source_total` допустим** одним обходом.
   Второй проход ради count не делать: если candidate уже `N+1`-детектит
   полноту, а точное M нужно — совмещать со сбором, не отдельным SRF.
4. `read_state` уже различает `source_limit` / `permission` /
   `read_failure`; `unknown_total` уже несёт «total неточный». Новых
   полей формата не требуется — только честное заполнение.

### Доведение до UI

`collected` уже в health. Довести полную тройку до статуса вкладки:

1. Reader выставляет per-section `SourcePopulation{ collected, total,
   total_quality }` для всех четырёх секций (механизм есть — сейчас
   питается coverage-фактом; убедиться, что per-section, а не только
   агрегат источника).
2. API отдаёт статус секции: `collected`, `source_total` (или его
   отсутствие при `unknown_total`), `total_quality`, `read_state`. Место
   — статус вкладки рядом со счётчиком строк (в терминах web-API спеки —
   поле секции в summary-ручке).
3. UI показывает: `N` (добрано всё), `N из M` (точное), `N+` /
   «добрано N, есть ещё» (обрезано, total неточный), плюс причину из
   `read_state` (лимит источника / нет прав / ошибка чтения).

## Развилка (зафиксирована)

Для ossc pg_store_plans точного дешёвого `source_total` нет. Решение —
**не платить за него**: LowerBound через `N+1`, `unknown_total=true`.
Показывать «добрано N, есть ещё» без точного M честнее, чем удерживать
блокировку текстов ради числа. Это согласуется с доктриной видимой
деградации: `total_quality=LowerBound` — не скрытая неточность, а
объявленная.

## Тестирование

- Сбор: golden-запросы `source_total` не содержат `pg_stat_statements`
  без `(false)` и `pg_store_plans` без `(false)` для vadv; ossc-путь не
  содержит `count(*) FROM pg_store_plans`. Юнит на классификатор
  read_state/unknown_total по числу возвращённых строк (`N` vs `N+1`).
- Reader: per-section `SourcePopulation` для каждой из четырёх секций;
  инвариант `collected ≤ total`; `total_quality=LowerBound` при
  `unknown_total`.
- API: статус вкладки несёт `collected`/`source_total`/`read_state`;
  при обрезке `source_total` помечен неточным.

## Не цели

- Точное M для ossc pg_store_plans (недостижимо дёшево — осознанно
  LowerBound).
- Новые поля формата: `SnapshotCoverageV1` уже достаточен.
- Оценка через `reltuples` для tables/indexes: их `count(*)` дёшев,
  приблизительность не нужна.
- Подсветка/цвет статуса — отдельно (каталог порогов, PR #140).

## Порядок реализации

1. Сбор: убрать чтение текстов в `source_total` (statements(false),
   ossc-plans без count), `N+1`-детект полноты, честный
   `read_state`/`unknown_total`. Юнит-тесты запросов.
2. Reader: per-section `SourcePopulation` для четырёх секций.
3. API: статус секции (collected/total/quality/read_state) в
   summary-ручке.
4. UI: рендер «N», «N из M», «N+», причина.

Шаг 1 самоценен (чинит существующую блокировку текстов), 2–4
последовательно доводят статус до экрана. Всё — один PR: одна фича.
