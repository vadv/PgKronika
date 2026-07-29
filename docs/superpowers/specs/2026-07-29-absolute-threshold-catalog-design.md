# Типизированный каталог абсолютных порогов

Дата: 2026-07-29.

Статус: на ревью перед планированием реализации.

Документ уточняет контракт Класса 1 из исследования
`2026-07-29-anomaly-highlight-research.md`. Первый PR реализует
аналитическое ядро и не менее половины фактического каталога исследования,
но не подключает результат к web API.

## Контекст

В `crates/kronika-analytics/src/anomaly/` уже реализован Класс 2:
modified z-score относительно истории конкретной серии. Класс 1 отвечает
на другой вопрос: пересекло ли наблюдение заранее заданную границу,
привязанную к ресурсу, лимиту или операторской политике.

Исследование содержит 64 числовые строки и 5 строк для ошибок и событий.
Заявление о приблизительно 120 метриках не подтверждается содержимым
таблиц. Некоторые строки объединяют несколько самостоятельных метрик,
например `r/w_await` и `errors/drops`. Первый PR разворачивает такие строки
в отдельные типизированные записи и реализует 42 логические метрики.

`GET /v1/frame/{view}` ещё не реализован. Поэтому первый PR создаёт
самодостаточное ядро и каталог без web-маппинга, DTO, OpenAPI и клиентской
раскраски.

## Цели

- Добавить детерминированную, не выполняющую I/O классификацию Класса 1
  в `kronika-analytics`.
- Сделать форму входа, единицу измерения, оператор сравнения и пороги
  каждой метрики проверяемой частью типизированного каталога.
- Реализовать 42 логические метрики из доменов CPU, памяти, PSI, cgroup,
  диска, сети и PostgreSQL tables/vacuum.
- Возвращать объяснимый вердикт со всеми числами, которые участвовали
  в классификации.
- Явно отличать отсутствие данных, неприменимость правила, некорректное
  число и несовместимую форму входа.
- Сохранить постоянную память и постоянное время одной классификации.

## Нецели

- Подключение каталога к `/v1/frame/{view}` или другому HTTP API.
- Изменение Класса 2, `/v1/anomalies` или incident lenses.
- Классификация строковых состояний, log severity и event category.
- Настройка порогов во время выполнения.
- Калибровка стартовых значений на демостенде.
- Перенос всех оставшихся строк исследования в первый PR.
- Добавление отсутствующих collector-метрик или UI-проекций.

## Решение

Выбран декларативный типизированный каталог. Общие классификаторы
реализуются один раз, а каждая логическая метрика связывает стабильный
`MetricId` с типизированной `Policy`.

Отклонённые варианты:

- отдельная функция для каждой метрики дублирует сравнения и затрудняет
  единообразную проверку границ;
- таблица строк и чисел, загружаемая во время выполнения, не позволяет
  компилятору проверить форму
  входа и переносит ошибки конфигурации в выполнение.

Каталог знает логические метрики, но не знает PGM sections, registry type
IDs, HTTP routes или UI columns. Будущий web-адаптер сопоставит колонку
проекции с `MetricId` и подготовит требуемую форму входа.

## Структура модуля

```text
crates/kronika-analytics/src/threshold/
├── mod.rs
├── model.rs
├── policy.rs
└── catalog/
    ├── mod.rs
    ├── cpu.rs
    ├── memory.rs
    ├── pressure.rs
    ├── cgroup.rs
    ├── storage.rs
    └── postgres_tables.rs
```

`model.rs` определяет публичные результаты и входные значения.
`policy.rs` содержит общие классификаторы. Доменные файлы содержат только
`MetricId`, `Policy` и стартовые числа.

Публичные типы и функции реэкспортируются из
`crates/kronika-analytics/src/lib.rs`. Английский и русский README crate
получают одинаковое описание контракта.

## Модель результата

```rust
pub enum Level {
    Inactive,
    Ok,
    Warning,
    Critical,
}

pub enum Classified {
    Verdict(Verdict),
    NotClassified(NotClassifiedReason),
}

pub struct Verdict {
    pub level: Level,
    pub boundary: Option<Boundary>,
    pub evidence: Evidence,
}

pub struct Boundary {
    pub operator: Comparison,
    pub value: f64,
}

pub enum Comparison {
    Above,
    AtLeast,
    Below,
    AtMost,
}

pub enum NotClassifiedReason {
    Missing,
    NonFinite,
    OutOfDomain,
    InvalidDenominator,
    NotApplicable,
    InputShapeMismatch,
}
```

`Comparison` сохраняет различие между строгими и нестрогими границами.
`boundary` содержит границу, которая определила `warning` или `critical`.
Для `ok` и `inactive` поле равно `None`.

`Evidence` — закрытый enum с вариантами для каждой формы входа:

- `Scalar`: наблюдаемое значение;
- `Fraction`: числитель, знаменатель и вычисленная доля;
- `RatioWithFloor`: доля, абсолютный счётчик и floor;
- `Age`: исходная временная отметка, `now`, вычисленный возраст и gate;
- `FreeCapacity`: доступные и общие байты, доступная доля и абсолютный
  потолок.

Такой результат позволяет будущему адаптеру построить подсказку без
повторного вычисления. Все варианты имеют фиксированный размер и не
владеют строками или коллекциями.

## Модель входа

```rust
pub enum MetricInput {
    Missing,
    NotApplicable,
    Scalar(f64),
    Fraction {
        numerator: f64,
        denominator: f64,
    },
    RatioWithFloor {
        ratio: f64,
        count: f64,
    },
    Age {
        epoch_seconds: f64,
        now_seconds: f64,
        gate: bool,
    },
    FreeCapacity {
        available_bytes: f64,
        total_bytes: f64,
    },
}
```

`Missing` и `NotApplicable` являются входными состояниями, а не
специальными значениями `f64`. Любой `NaN` или infinity возвращает
`NonFinite`. Отрицательное значение возвращает `OutOfDomain` для
политики с неотрицательным доменом; ядро не зажимает значение в допустимый
диапазон.

Нулевой знаменатель и отрицательный знаменатель возвращают
`InvalidDenominator`. Форма входа, не совпадающая с `Policy`, возвращает
`InputShapeMismatch`: это диагностирует ошибку будущего адаптера, не
маскируя её как отсутствие данных.

## Политики

```rust
pub enum Policy {
    Scalar(ScalarPolicy),
    Fraction(FractionPolicy),
    RatioWithFloor(RatioWithFloorPolicy),
    AgeGated(AgePolicy),
    FreeCapacity(FreeCapacityPolicy),
}
```

`ScalarPolicy` задаёт направление ухудшения, необязательные warning и
critical boundaries, неотрицательный домен и поведение нуля. Остальные
политики вычисляют производное значение и применяют те же точные
границы.

Поддерживаемые архетипы:

| Политика | Вычисление |
| --- | --- |
| `Scalar` | Сравнивает одно число с верхними или нижними границами. |
| `Fraction` | Делит числитель на положительный знаменатель и сравнивает долю. |
| `RatioWithFloor` | Классифицирует долю только при пересечении абсолютного floor. |
| `AgeGated` | Вычисляет `now - epoch` только при истинном gate. |
| `FreeCapacity` | Требует одновременно низкую доступную долю и малый абсолютный остаток. |

Проверка всегда начинает с critical boundary, затем проверяет warning
boundary. Поэтому пересечение обеих границ даёт `Critical`.

`zero -> inactive` не является общим правилом. Каждая политика явно
задаёт `ZeroDisposition::Classify` или `ZeroDisposition::Inactive`.

Поля политик закрыты. Проверяемые конструкторы запрещают `NaN`, infinity,
неверный порядок границ, отрицательные floor и несовместимое направление.
Статический каталог строится только из валидированных значений.

## Идентификаторы и метаданные

```rust
pub struct CatalogEntry {
    pub id: MetricId,
    pub policy: Policy,
    pub unit: Unit,
    pub calibration: Calibration,
}

pub enum Calibration {
    Provisional,
    Validated,
}
```

`MetricId` — закрытый enum с уникальным стабильным строковым кодом.
Строковый код предназначен для диагностики, golden-тестов и будущего
маппинга, но не совпадает автоматически с registry section или UI column.

Все значения первого PR получают `Calibration::Provisional`: исследование
называет их стартовыми значениями и требует калибровки. Наличие политики в
каталоге не утверждает, что порог уже подтверждён эксплуатационными
данными PgKronika.

`Unit` фиксирует единицу классифицируемого значения: `Percent`, `Ratio`,
`Count`, `Kibibytes`, `Milliseconds`, `Seconds`, `CountPerSecond`,
`BytesPerSecond` или `Bytes`. Для составного `Evidence` единицы операндов
определяет вариант политики.

## Представление значений

Scalar percent передаётся в диапазоне `0..=100`: например, 70 % имеет
значение `70.0`. Ядро не ограничивает верхнюю границу, поскольку отдельная
метрика CPU или ошибочный источник могут вернуть больше 100 %; отрицательное
значение остаётся ошибкой домена.

`Fraction` и `RatioWithFloor` используют долю: `1.0` соответствует 100 %.
Поэтому границы `load1 / cores` равны `1.0` и `2.0`, а границы
`dead_tuple_pct` равны `0.10` и `0.20`.

`FreeCapacity` получает байты, вычисляет долю `available / total` и
сравнивает её с `0.20` и `0.10`. Вычисленная доля сохраняется в
`Evidence::FreeCapacity`.

Пороговые размеры задаются в двоичных единицах. Каталог хранит уже
переведённые значения, поэтому классификатор не выполняет преобразование
единиц.

## Семантика нуля

По умолчанию ноль классифицируется как `Ok`. Ноль получает `Inactive`
только для метрик, где он означает отсутствие активности или события:

- `os.process.virtual_growth_kib`;
- `os.process.resident_growth_kib`;
- `os.process.virtual_swap_kib`;
- `os.memory.swap_used_kib`;
- `os.vmstat.swap_in_per_second`;
- `os.vmstat.swap_out_per_second`;
- `os.process.major_faults_delta`;
- `os.cgroup.cpu_throttled_ms_delta`;
- `os.cgroup.cpu_throttle_events_delta`;
- `os.cgroup.memory_oom_kills_delta`;
- `os.process.block_delay_seconds_delta`;
- `os.disk.blocks_read_per_second`;
- `os.network.errors_per_second`;
- `os.network.drops_per_second`;
- `pg.tables.temp_bytes_per_second`.

Для age policy ложный gate возвращает `NotApplicable`, а не `Inactive`.
Для `RatioWithFloor` значение, не пересёкшее абсолютный floor,
классифицируется как `Ok`.

## Состав первого каталога

Таблица задаёт нормативный объём первого PR. `warn` и `crit` сохраняют
строгий или нестрогий оператор из исследования. `-` означает отсутствие
уровня.

### CPU и load

| `MetricId` | Вход | warn | crit |
| --- | --- | ---: | ---: |
| `os.process.cpu_pct` | scalar, % | `>= 50` | `>= 90` |
| `os.load.avg1_per_core` | fraction | `> 1` | `> 2` |
| `os.cpu.idle_pct` | scalar, % | `< 30` | `< 10` |
| `os.cpu.iowait_pct` | scalar, % | `> 5` | `> 15` |
| `os.cpu.steal_pct` | scalar, % | `> 3` | `> 10` |
| `os.load.procs_blocked` | scalar, count | `> 0` | `> 4` |
| `pg.activity.backend_load_per_core` | fraction | `>= 0.25` | `>= 0.5` |

### Память и swap

| `MetricId` | Вход | warn | crit |
| --- | --- | ---: | ---: |
| `os.memory.used_pct` | scalar, % | `>= 70` | `>= 90` |
| `os.process.virtual_growth_kib` | scalar, KiB | `> 102400` | `> 1048576` |
| `os.process.resident_growth_kib` | scalar, KiB | `> 102400` | `> 1048576` |
| `os.process.virtual_swap_kib` | scalar, KiB | `> 0` | `> 102400` |
| `os.memory.swap_used_kib` | scalar, KiB | `> 0` | `> 1048576` |
| `os.vmstat.swap_in_per_second` | scalar, per-second | `-` | `> 0` |
| `os.vmstat.swap_out_per_second` | scalar, per-second | `-` | `> 0` |
| `os.process.major_faults_delta` | scalar, count | `> 100` | `> 10000` |
| `os.process.rss_kib` | scalar, KiB | `> 1048576` | `> 4194304` |

Размеры используют двоичные единицы: `100 MiB = 102400 KiB`,
`1 GiB = 1048576 KiB`.

### PSI

| `MetricId` | Вход | warn | crit |
| --- | --- | ---: | ---: |
| `os.psi.cpu_some_pct` | scalar, % | `>= 5` | `>= 25` |
| `os.psi.memory_some_pct` | scalar, % | `>= 5` | `>= 25` |
| `os.psi.io_some_pct` | scalar, % | `>= 10` | `>= 40` |

### cgroup

| `MetricId` | Вход | warn | crit |
| --- | --- | ---: | ---: |
| `os.cgroup.cpu_used_pct` | scalar, % | `>= 70` | `>= 90` |
| `os.cgroup.cpu_throttled_ms_delta` | scalar, ms | `> 0` | `> 100` |
| `os.cgroup.cpu_throttle_events_delta` | scalar, count | `> 0` | `-` |
| `os.cgroup.memory_anon_pct` | scalar, % | `>= 70` | `>= 90` |
| `os.cgroup.memory_headroom_pct` | scalar, % | `< 20` | `< 10` |
| `os.cgroup.memory_oom_kills_delta` | scalar, count | `-` | `> 0` |

### Диск и сеть

| `MetricId` | Вход | warn | crit |
| --- | --- | ---: | ---: |
| `os.disk.util_pct` | scalar, % | `>= 60` | `>= 90` |
| `os.disk.max_await_ms` | scalar, ms | `>= 2` | `>= 10` |
| `os.disk.read_await_ms` | scalar, ms | `>= 2` | `>= 10` |
| `os.disk.write_await_ms` | scalar, ms | `>= 2` | `>= 10` |
| `os.filesystem.free_capacity` | available/total bytes | `< 20 %` и `< 15 GiB` | `< 10 %` и `< 15 GiB` |
| `os.process.block_delay_seconds_delta` | scalar, s | `> 10` | `> 50` |
| `os.disk.blocks_read_per_second` | scalar, per-second | `> 0` | `-` |
| `os.network.errors_per_second` | scalar, per-second | `> 0` | `> 10` |
| `os.network.drops_per_second` | scalar, per-second | `> 0` | `> 10` |

`FreeCapacity` использует `15 GiB = 16106127360 bytes`. Равенство
абсолютному потолку не пересекает правило, поскольку исходный контракт
задаёт `< 15 GB`.

### PostgreSQL tables и vacuum

| `MetricId` | Вход | warn | crit |
| --- | --- | ---: | ---: |
| `pg.tables.dead_tuple_pct` | ratio + dead count | `>= 10 %` и `> 10000` | `>= 20 %` и `> 10000` |
| `pg.tables.dead_tuples` | scalar, count | `>= 1000` | `>= 100000` |
| `pg.tables.sequential_scan_pct` | scalar, % | `>= 30` | `>= 80` |
| `pg.tables.modified_since_analyze` | scalar, count | `>= 100000` | `>= 1000000` |
| `pg.tables.inserted_since_vacuum` | scalar, count | `>= 100000` | `>= 1000000` |
| `pg.tables.autovacuum_age_seconds` | age, gate `dead > 0` | `> 21600` | `> 86400` |
| `pg.tables.autoanalyze_age_seconds` | age, gate `modified >= 10000` | `> 21600` | `> 86400` |
| `pg.tables.temp_bytes_per_second` | scalar, per-second | `> 0` | `-` |

Для `dead_tuple_pct` warning использует проверенный в исследовании
абсолютный floor `dead > 10000`, а не исходный reftool warning `>= 5 %`
без floor. Critical сохраняет `>= 20 %` и применяет тот же floor.

`temp_bytes_per_second` остаётся provisional indicator: исследование
прямо отмечает отсутствие универсальной промышленной отсечки.

## Инварианты

- `CATALOG` содержит ровно 42 записи в первом PR.
- `MetricId` и строковые коды уникальны.
- Порядок каталога стабилен и не зависит от hash iteration.
- Каждая запись содержит валидную политику и единицу.
- Все записи первого PR помечены `Provisional`.
- Классификация не выполняет I/O, не читает время и не выделяет память.
- `AgeGated` получает `now_seconds` от вызывающего кода.
- Неприменимое правило не превращается в `Ok`.
- Некорректный операнд не превращается в `Inactive`.
- Равенство обрабатывается только оператором `AtLeast` или `AtMost`.
- Critical имеет приоритет над warning.

## Ошибки и граничные случаи

| Состояние | Результат |
| --- | --- |
| `MetricInput::Missing` | `NotClassified(Missing)` |
| `MetricInput::NotApplicable` | `NotClassified(NotApplicable)` |
| Любой `NaN` или infinity | `NotClassified(NonFinite)` |
| Отрицательное значение в неотрицательном домене | `NotClassified(OutOfDomain)` |
| Знаменатель `<= 0` | `NotClassified(InvalidDenominator)` |
| Ложный gate для age | `NotClassified(NotApplicable)` |
| `epoch > now` | `NotClassified(OutOfDomain)` |
| Форма входа не соответствует политике | `NotClassified(InputShapeMismatch)` |

Переполнение не возникает: классификаторы не преобразуют входной `f64`
в целые типы и проверяют конечность каждого вычисленного значения.

## Память и производительность

Каталог — статические массивы и значения фиксированного размера. Одна
классификация выполняет ограниченное число сравнений и не создаёт
`Vec`, `String`, map или heap-backed error. Пиковая дополнительная память
равна размеру нескольких enum и scalar locals, то есть O(1).

Размер входа не управляет числом операций или объёмом памяти. Каталог не
копируется на запрос и возвращается как `&'static [CatalogEntry]`.

## Тестирование

### Unit tests классификаторов

Для каждого архетипа проверяются:

- значение ниже, на и выше warning boundary;
- значение ниже, на и выше critical boundary;
- приоритет critical;
- политика только с warning или только с critical;
- `ZeroDisposition::Classify` и `ZeroDisposition::Inactive`;
- missing, not applicable, non-finite и отрицательный вход;
- нулевой и отрицательный знаменатель;
- ложный age gate и `epoch > now`;
- точная семантика `<`, `<=`, `>`, `>=`.

### Catalog tests

- В каталоге ровно 42 записи.
- Все `MetricId` и строковые коды уникальны.
- Записи отсортированы в нормативном порядке.
- Все политики проходят повторную проверку инвариантов.
- Golden-таблица фиксирует ID, unit, calibration, форму входа и пороги.
- Каждая запись принимает representative input правильной формы без
  `InputShapeMismatch`.

### Workspace gates

- `cargo fmt --all --check`;
- `cargo clippy --workspace --all-targets --all-features -- -D warnings`;
- `cargo test --workspace`;
- `cargo run -p xtask -- check-deps`.

## Совместимость

Модуль добавляет новый Rust API и не меняет существующие JSON schema,
формат PGM/OVF или поведение endpoints. Изменять `MetricId`, строковые
коды или семантику границ после подключения web можно только с явным
пересмотром projection contract.

До появления потребителя код считается базовым API. README обязан прямо
сообщать, что каталог ещё не подключён к HTTP и UI.

## Критерии приёмки

- Публичный `threshold` API компилируется без новых зависимостей.
- Каталог содержит 42 записи из нормативной таблицы.
- Все вердикты объяснимы через `Boundary` и `Evidence`.
- Ни одна классификация не выделяет heap memory и не выполняет I/O.
- Граничные операторы совпадают с таблицей.
- Все ошибки входа возвращают точную `NotClassifiedReason`.
- README на английском и русском описывают одинаковый контракт и
  ограничение «без web consumer».
- Все workspace gates проходят.

## Последующие работы

Следующий этап добавит оставшиеся числовые политики и отдельно разберёт
категориальные правила. Подключение к web начнётся только вместе с
`GET /v1/frame/{view}`: projection catalog получит явный `MetricId`, а
frame rows — опциональный verdict. Клиент будет применять готовый
`level`, не повторяя пороги.
