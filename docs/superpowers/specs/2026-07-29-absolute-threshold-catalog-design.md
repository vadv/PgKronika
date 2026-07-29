# Каталог абсолютных порогов (Класс 1) в аналитическом ядре

Дата: 2026-07-29. Базируется на main после #138. Реализует Класс 1 из
исследования подсветки (2026-07-29-anomaly-highlight-research.md): пороги,
привязанные к физическим и протокольным лимитам, как чистые функции ядра.
Класс 2 (baseline, modified z-score) уже существует в
`crates/kronika-analytics/src/anomaly/` и здесь не трогается.

## Задача

Значения в таблицах интерфейса нужно классифицировать по уровню:
`critical`, `warning`, `ok`, `inactive`. Часть метрик имеет универсальный
«плохой» порог (saturation ресурса, приближение к лимиту) — их и
покрывает этот каталог. Вычисление — в ядре: клиент красит ячейку по
готовому вердикту, порогов не считает. Вердикт объясним: несёт порог и
фактическое значение, чтобы интерфейс показал «crit: dead% 22.4 > 20».

## Место и дисциплина

Новый модуль `crates/kronika-analytics/src/threshold/`, симметричный
`anomaly/`:

- **детерминированный, I/O-free**: вход — значение (± параметры инстанса
  из снапшота), выход — вердикт; никакой сети, файлов, времени;
- **знает только числа**: как и `anomaly`, ядро не знает про секции и
  источники — маппинг «колонка → классификатор» живёт у вызывающего
  (web-слой), ядро принимает значение и параметры;
- **несёт все числа для объяснения вердикта** — по образцу `Evaluated`,
  которая отдаёт `med_cur/med_ref/mad_ref/…`.

Уровни — свой enum, не переиспользовать `overview::counts::Severity`
(та про лог-severity Fatal/Panic). Класс 1 живёт рядом с Класс 2, но не
смешивается: z-score отвечает «отклонилось от своей нормы», порог —
«превысило универсальный лимит».

## Типы

По образцу `Scored`/`Evaluated`/`NotEvaluatedReason`:

```rust
pub enum Level { Inactive, Ok, Warning, Critical }

/// Verdict for one value against an absolute threshold.
pub enum Classified {
    /// Value classified; carries every number to explain the level.
    Verdict(Verdict),
    /// Value could not be classified (missing/non-finite input).
    NotClassified(NotClassifiedReason),
}

pub struct Verdict {
    pub level: Level,
    /// The observed value that produced the level.
    pub observed: f64,
    /// The nearest crossed boundary (warn or crit), if any.
    pub boundary: Option<f64>,
    /// Which comparison fired (Above / Below / Equals), for phrasing.
    pub direction: Compare,
}

pub enum Compare { Above, Below, Equals }
pub enum NotClassifiedReason { Missing, NonFinite, NotApplicable }
```

`Verdict` — минимальный контракт объяснимости: уровень + фактическое +
пересечённая граница + направление. Интерфейс из него строит tooltip.
Для Класса 2 объяснение остаётся за `Evaluated` (baseline/z-score);
web-слой выбирает, какой вердикт применить к колонке.

## Архетипы классификаторов

Каталог ~120 метрик (полный список — исследование #140) сводится к
небольшому числу архетипов. Каждый — чистая функция `f64 (+params) →
Classified`. Универсальные принимают только значение; config-bound —
ещё параметр из снапшота `pg_settings`.

```rust
/// Higher is worse: ok < warn ≤ crit. Zero → Inactive.
fn high(v: f64, warn: f64, crit: f64) -> Classified;

/// Higher is better (hit ratios): ok ≥ good, warn ≥ mid, else crit.
fn hit(v: f64, good: f64, warn: f64) -> Classified;

/// Duration with a lower ok band; zero → Inactive.
fn duration(v: f64, warn: f64, crit: f64) -> Classified;

/// Ratio gated by an absolute floor (dead%: ratio≥R AND count>floor).
fn ratio_floor(ratio: f64, count: f64, r: f64, floor: f64) -> Classified;

/// Fraction of a config limit (connections vs max_connections;
/// wraparound age vs autovacuum_freeze_max_age at 50%/80%).
fn limit_fraction(v: f64, limit: f64, warn_frac: f64, crit_frac: f64)
    -> Classified;

/// Age since an epoch, gated by a companion predicate (autovacuum
/// staleness only when dead tuples present). `now` is passed IN — the
/// kernel stays time-free.
fn age_gated(epoch: f64, now: f64, gate: bool, warn_s: f64, crit_s: f64)
    -> Classified;
```

Config-bound функции берут лимит параметром, не читают конфиг сами:
вызывающий достаёт `max_connections`, `autovacuum_freeze_max_age`,
`autovacuum_vacuum_scale_factor` из снапшота и передаёт. Так ядро
остаётся не знающим про источники.

Context-aware (A* в каталоге) — не отдельный архетип, а параметр
вызова: `query_duration` классифицируется только если `state` не idle;
`disk.space` — доля от `total`. Гейт вычисляет вызывающий, ядру передаёт
готовые числа либо `NotApplicable`.

## Каталог: значения

Числа, классы и config-привязка — в исследовании #140 (полный каталог по
доменам: CPU/load, память/swap, PSI, cgroup, диск/IO, сеть, сессии,
кэш/checkpoint, таблицы/vacuum, statements/plans, errors, replication).
Спека фиксирует: каждая строка того каталога реализуется одним из
архетипов выше; числа — стартовые дефолты (tunable), финализируются
калибровкой на демо-стенде.

Разделение по архетипам (примеры, не исчерпывающе):

- `high`: cpu_pct, mem_pct, iow_pct, disk.util_pct, PSI *_some_pct,
  cgroup_*_pct, seq_pct, dead_pct-как-ratio-часть.
- `hit`: hit_pct, io_hit_pct, effective_hit_pct, scan_efficiency.
- `duration`: query_duration_s, xact_duration_s, elapsed_s.
- `ratio_floor`: dead tuples (ratio≥0.1 AND >10k), n_dead_tup.
- `limit_fraction`: connections vs `max_connections`; wraparound age vs
  `autovacuum_freeze_max_age` (50%/80%).
- `age_gated`: last_autovacuum / last_autoanalyze staleness.

Класс 2 (не в этом модуле): `time_ratio`, `cv` и прочие «относительно
своего среднего» — идут через существующий `anomaly::score_window`.

## Контракт выдачи в web и frame

Ядро отдаёт `Classified` на значение. Web-слой:

1. держит маппинг «секция.колонка → классификатор + его параметры»
   (аналог того, что frame-спека #134 называет «вердикты только для
   колонок с реализованными линзами» — теперь для колонок с порогом);
2. на каждую ячейку ответа frame прикладывает `level` + объяснение
   (`observed`, `boundary`), если классификатор для колонки есть;
   колонка без классификатора едет без вердикта;
3. клиент красит ячейку по `level`, tooltip строит из объяснения.

Это не меняет транспорт frame — добавляет опциональное поле вердикта к
ячейке. Класс 1 (порог) и Класс 2 (baseline) приходят одним полем `level`
с разным объяснением; клиенту всё равно, чем вердикт рождён.

## Тестирование

Классификаторы — чистые функции, обязаны иметь юнит-тесты (правило
проекта): для каждого архетипа — таблица «вход → ожидаемый уровень» на
границах (ниже warn, на warn, между, на crit, выше; zero → Inactive;
non-finite → NotClassified). Config-bound — тест на разных лимитах
(порог сдвигается с `max_connections`). Golden-таблица дефолтов каталога
фиксирует стартовые числа, чтобы их изменение было видимым в diff.

## Не цели

- Класс 2 / baseline / z-score — уже реализованы, не трогаются.
- Семантические инциденты («checkpoint storm», «lock cascade») — это
  слой линз, отдельно от per-cell порогов.
- Тюнинг порогов пользователем в рантайме — дефолты компилируемые;
  конфигурируемость (если понадобится) — отдельная работа.
- Калибровка числовых отсечек PSI и решение по lock-wait/temp
  (Класс 1 vs Класс 2) — на демо-стенд, вне этой спеки.
- Раскраска на клиенте — клиент только применяет `level`.

## Порядок реализации

1. Модуль `threshold/`: `Level`, `Classified`, `Verdict`, архетипы
   `high`/`hit`/`duration`/`ratio_floor`/`limit_fraction`/`age_gated`
   с юнит-тестами.
2. Каталог дефолтов: таблица «метрика → архетип → числа», golden-тест.
3. Web-маппинг «колонка → классификатор» и поле вердикта в ответе frame.
4. Клиент применяет `level` к ячейке (вне ядра, отдельным шагом).

Шаги 1–2 самодостаточны и дают тестируемое ядро без потребителя-ручки;
шаг 3 подключает его к frame, когда та ручка реализуется.
