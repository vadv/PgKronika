# Просмотр OVF web-index через pg_kronika-dump

Дата: 2026-07-28.

## Цель

`pg_kronika-dump` должен читать самостоятельный файл OVF и показывать его
физическую структуру и логическое содержимое web-index без запуска web-процесса
и без доступа к исходному PGM.

Инструмент остаётся read-only: он не перестраивает индекс, не исправляет файл и
не изменяет дерево данных.

## Определение режима

Режим обычного файла определяется по его имени:

| Имя | Режим |
| --- | --- |
| `*.pgm` | PGM |
| `*.ovf` | OVF |
| любое другое | журнал |

Суффиксы регистрозависимы. После выбора режима соответствующий декодер проверяет
magic, framing, версии, лимиты и CRC. Неверное расширение не приводит к
эвристическому переключению на другой декодер.

Каталог, как и раньше, включает режим дерева. Символические ссылки и иные типы
файлов не принимаются.

```text
pg_kronika-dump <path> [--rows] [--limit N]
```

Без `--rows` режим OVF читает header и directory, но не декодирует тела блоков.
С `--rows` он также декодирует `UiSummary` и все `EntitySeries`. Параметр
`--limit N` требует `--rows`, ограничивает число series внутри каждого metric и
по умолчанию равен 1000. Фактический wire-лимит top-K не превышает 64, но общий
CLI-контракт остаётся одинаковым для PGM и OVF.

## Reader API

Канонический wire-парсер остаётся в `kronika-reader`. Reader получает публичный
read-only API автономной инспекции OVF:

- API читает identity из header самого файла;
- проверяет физический header, directory, известные flags, codec, размеры и CRC;
- не требует expected identity или sibling PGM;
- при чтении тела применяет существующие `Bounds` и декодеры блоков;
- не ослабляет production API, который проверяет OVF относительно ожидаемой
  идентичности исходного PGM.

`pg_kronika-dump` не копирует смещения, wire-константы или zstd-логику из
reader.

Автономная инспекция подтверждает внутреннюю целостность OVF, но не доказывает,
что файл принадлежит лежащему рядом PGM. Это ограничение явно описывается в
README.

## JSON без --rows

Успешный результат содержит один JSON-объект:

```json
{
  "kind": "ovf",
  "path": "/var/lib/pg_kronika/2026/07/28/1785200000000000.ovf",
  "file_bytes": 84520,
  "header": {
    "fact_schema_version": 1,
    "extractor_semantics_version": 7,
    "registry_contract_version": 1,
    "source_format_version": 1,
    "pgm_source_id": 7,
    "source_min_ts_us": 1785200000000000,
    "source_max_ts_us": 1785203600000000,
    "source_file_len": 923410,
    "source_descriptor": "001122...",
    "fact_key": "aabbcc...",
    "segment_lineage_id": "ddeeff...",
    "directory_count": 19
  },
  "blocks": [
    {
      "kind": "ui_summary",
      "kind_code": 10,
      "logical_id": 0,
      "schema_version": 1,
      "codec": "none",
      "sorted": true,
      "stored_bytes": 1400,
      "decoded_bytes": 1400,
      "items": 9,
      "min_ts_us": 1785200000000000,
      "max_ts_us": 1785203600000000
    }
  ]
}
```

Точные числовые версии и kind codes берутся из файла. Двоичные идентификаторы
выводятся в lowercase hex фиксированной длины. Неизвестный block kind
сохраняется в directory как `kind: null` и не декодируется.

`file_bytes` сверяется с длиной из header. `blocks` сохраняет канонический
порядок directory и включает не только web-index, но и остальные OVF-блоки,
чтобы физический состав файла был виден полностью.

## JSON с --rows

При `--rows` каждый известный web-index block получает поле `content`.

### UiSummary

Содержимое включает:

- grid: `start_us`, `bucket_width_s`, `bucket_count`;
- полный массив `snapshot_times_us`;
- views в каноническом порядке;
- для каждого view: `view_code`, revision, status, population по снимкам и
  notable по снимкам.

`null` сохраняет отсутствующее значение и не заменяется нулём.

### EntitySeries

Содержимое включает:

- `view_code`, view revision, identity revision и status;
- observed range и grid;
- coverage как массив `true`/`false` длиной `bucket_count`;
- полный dictionary: `entity_ref`, key в lowercase hex и label;
- metrics в каноническом порядке;
- для каждого metric: code, revision, flags, unit code, aggregation, status,
  cutoff score и series;
- для каждой возвращённой series: `entity_ref`, dictionary key и label, exact
  score, max bucket value и `values`.

`values` имеет ровно `bucket_count` элементов. Наблюдаемое нулевое значение
выводится как `0.0`, отсутствующее значение — как `null`.

`--limit N` применяется отдельно к series каждого metric. Поле
`truncated: true` означает, что в metric есть дополнительные series. Dictionary
выводится полностью даже при ограничении, чтобы все ссылки оставались
самодостаточными.

Бинарный entity key не интерпретируется dump: hex сохраняет точные typed
identity bytes, а label даёт человекочитаемое представление.

## Ошибки и ограничения

Ошибка имени, чтения, framing, версии, CRC, codec, размера или логического
декодирования даёт диагностику в stderr и exit code 1. Ошибка аргументов даёт
exit code 2. Частичный JSON не печатается.

Режим наследует все текущие OVF `Bounds`. Без `--rows` тела блоков не
распаковываются. С `--rows` тела читаются и проверяются последовательно, но
итоговая JSON-модель содержит все выбранные web-index series до сериализации.
Её размер ограничен OVF `Bounds` и `--limit`; dump не загружает sibling PGM.

## Тесты

Реализация покрывает:

1. Выбор PGM, OVF и journal по имени файла.
2. Metadata-only OVF: header и весь directory без `content`.
3. `--rows`: точный `UiSummary`, dictionary, coverage, metric metadata,
   reconstructed bucket values и различие missing/zero.
4. `--limit`: независимое усечение series каждого metric и
   `truncated: true`.
5. Неизвестный block kind в metadata-only режиме.
6. Ошибки magic, CRC, несовместимой версии и превышения `Bounds`.
7. Отсутствие чтения sibling PGM.
