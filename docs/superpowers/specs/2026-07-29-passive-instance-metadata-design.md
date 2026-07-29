# Пассивная `instance_metadata`

Дата: 2026-07-29
Статус: `APPROVED`

## Цель

`instance_metadata` остаётся справочной секцией с фактами о PostgreSQL и ОС,
наблюдавшимися во время сбора. Ни наличие секции, ни значения её полей не
управляют допуском файлов, построением инцидентов, корреляцией сущностей,
непрерывностью plan-анализов или reset epoch метрик.

Из проекта полностью удаляются строки, собранные как
`"node_self_" + "id"` и `"KRONIKA_NODE_SELF_" + "ID"`. Запрет охватывает код,
схемы, тесты, документацию, конфигурацию и CI.

## Причина

Один data root является границей набора расследования. Все выбранные валидные
секции должны участвовать в анализе независимо от hostname, места чтения файлов
и изменений справочных метаданных во времени.

Прежний контракт требовал ровно одно node-label значение во всём диапазоне.
Отсутствие или различие значений прекращало построение инцидентов, меняло ключи
и запрещало часть entity joins. Это превращало необязательную описательную
метаданную в искусственный admission gate.

Дополнительные проверки boot epoch и PostgreSQL instance continuity относятся
к редким переходам и не оправдывают зависимость аналитики от всей служебной
секции. Для расследования сами сохранённые значения полезны, поэтому секция не
удаляется.

## Контракт секции

Тип `1_021_001` сохраняет логическое имя `instance_metadata` и одну строку в
каждой выпущенной секции. Точная текущая схема содержит:

```text
ts
hostname
pg_version_num
kernel_version
pg_system_identifier?
clock_ticks_per_sec
page_size_bytes
boot_id
btime
```

Поля являются только наблюдаемыми фактами:

- `hostname`, `kernel_version`, `boot_id` и `btime` описывают ОС;
- `pg_version_num` и `pg_system_identifier` описывают PostgreSQL;
- `clock_ticks_per_sec` и `page_size_bytes` фиксируют параметры, действовавшие
  во время сбора.

Коллектор продолжает писать секцию по расписанию и после принудительной
ротации. Специальная переменная окружения, config field, fallback на hostname и
дополнительная dictionary-строка удаляются.

Схема меняется на месте без legacy decoder, alias, reserved column или
compatibility layout. Старое тело именно этой секции не обязано декодироваться
новым точным контрактом. Остальные секции старого PGM остаются независимо
адресуемыми и должны использоваться запросами, которые их выбирают.

## Инциденты

`instance_metadata` удаляется из обязательного input set обработчика
инцидентов. Подготовка входа больше не строит множество node labels и не
возвращает ошибки отсутствующего или конфликтующего node label.

`EntityScope` удаляется. Entity joins ограничиваются существующими typed
identity, snapshot provenance и lifetime mapping. Отдельная проверка
описательного node label не выполняется.

Ключ результата получает новую внутреннюю версию:

```text
IncidentKeyV2 = (
  incident_start_us,
  incident_end_us,
  sorted EpisodeRefV1[]
)
```

Ключ не зависит от hostname, машины расследования или
`instance_metadata`. Все выбранные PGM и активные части участвуют в построении
эпизодов. Повреждение реально запрошенной payload-секции и действующие resource
bounds по-прежнему дают типизированную деградацию или ошибку.

## Plan-анализ

`instance_metadata` удаляется из `PLAN_CONTEXT_SECTIONS`. Удаляются
`InstanceContext`, conflict tracking этой секции и проверки:

- совпадения node label;
- совпадения major-версии из служебной секции;
- доступности и совпадения `pg_system_identifier`.

Непрерывность plan counters определяется текущими reset metadata, extension
version, `compute_query_id`, membership, coverage, gaps и идентичностью строки
плана. Удаляются quality counters, существовавшие только для прежних instance
checks. Справочные поля секции остаются доступными через generic section API и
dump, но не меняют результат plan-анализов.

## Overview-метрики

`instance_metadata` удаляется из allow-list источников metric extraction и из
`ResetTimeline`. `boot_id` и `btime` больше не создают специальный OS reset
epoch.

OS cumulative counters сохраняются как обычные серии без заявленного
`CgroupBoot` или `HostBoot` reset family. Для одной серии используется
стабильный внутренний epoch, выведенный из `MetricSeriesId`; уменьшение значения
остаётся reset, а известный пропуск остаётся gap. Потери
`MissingResetContext`, возникавшие только из-за отсутствия
`instance_metadata`, для этих факторов удаляются.

Цена решения принимается явно: если после reboot новое значение успело стать
не меньше предыдущего и между точками нет известного gap, редкий переход может
быть принят за обычную дельту. Этот риск не даёт служебной секции права
исключать данные из анализа.

## API и ошибки

Из incident data-quality ответа и problem registry удаляются состояния,
связанные только с отсутствующим или конфликтующим node label. Из plan quality
удаляются счётчики instance conflicts, boundaries, identity availability и
unsupported major version, если они не имеют другого производителя.

`instance_metadata` остаётся доступна через текущие generic section routes.
Новый отдельный endpoint не добавляется.

## Проверка

1. Repository guard и его unit tests доказывают отсутствие двух удалённых
   строк во всех tracked-файлах.
2. Codec round-trip подтверждает точную новую схему `instance_metadata`.
3. Collector BDD проверяет оставшиеся фактические поля без config override.
4. Incident regression строит результат без `instance_metadata` и использует
   payload из нескольких файлов.
5. Incident key не меняется при переносе тех же файлов в другой каталог.
6. Entity join tests не содержат node scope и сохраняют typed/snapshot
   ограничения.
7. Plan anomaly tests работают без `instance_metadata`; удалённые quality
   поля отсутствуют в точном JSON.
8. Overview tests доказывают value-decrease reset, gap и непрерывную OS-серию
   без boot metadata.
9. Qualification validators, formatting, strict clippy, workspace tests и
   dependency gate проходят на итоговом дереве.

## Вне объёма

- удаление самой `instance_metadata`;
- изменение сохранённых hostname, PostgreSQL, kernel или boot фактов;
- миграция и перепаковка существующих PGM;
- восстановление редких reboot/initdb границ другим эвристическим
  идентификатором;
- выбор файлов по любому полю `instance_metadata`.
