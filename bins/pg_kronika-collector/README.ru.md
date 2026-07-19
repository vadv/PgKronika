# pg_kronika-collector

[English version](README.md)

`pg_kronika-collector` — единственный процесс, который подключается к
PostgreSQL и пишет PGM. Он читает наступившие по расписанию источники
PostgreSQL, Linux, cgroup и при настройке stderr-журнал, добавляет одно
ограниченное окно в `active.parts`, затем запечатывает журнал в
`<first_timestamp>.pgm` при наступлении условия ротации.

Состояния `ready` и `sealed ...` идут в stdout, структурированные logfmt-события
— в stderr. Ошибка цикла сбора записывается в журнал, после чего демон повторяет
попытку. Ошибка конфигурации, первого подключения или открытия журнала
останавливает запуск.

## Обязательные настройки

| Переменная | Дефолт | Назначение |
| --- | ---: | --- |
| `KRONIKA_PG_DSN` | обязательна | URI или строка `key=value` для `tokio-postgres`. |
| `KRONIKA_OUT_DIR` | обязательна | Каталог с `active.parts` и готовыми `.pgm`. |
| `KRONIKA_SOURCE_ID` | `0` | `u64` в каталоге сегмента. Для нескольких коллекторов в общем каталоге задайте разные ненулевые значения. |
| `KRONIKA_LOG_LEVEL` | `info` | `error`, `warn`, `info`, `debug` или `trace`; неверное значение заменяется на `info`. |

Отсутствующий выходной каталог создаётся. Права файлов зависят от umask
процесса. В сегменты могут попасть SQL, планы, аргументы процессов и текст
журнала, поэтому доступ к каталогу нужно ограничить.

## Подключения и ограничения запросов

| Переменная | Дефолт |
| --- | ---: |
| `KRONIKA_PG_STATEMENT_TIMEOUT_MS` | `15000` |
| `KRONIKA_PG_LOCK_TIMEOUT_MS` | `1000` |
| `KRONIKA_PG_IDLE_IN_TX_TIMEOUT_MS` | `10000` |
| `KRONIKA_PG_EXCLUDE_DATABASES` | пусто; имена разделяются `;` |
| `KRONIKA_PG_POOL_REFRESH_SECS` | `600` |
| `KRONIKA_PG_HEAVY_TIMEOUT_CAP_MS` | `60000` |
| `KRONIKA_CYCLE_DB_BUDGET_MS` | `15000`; `0` выключает бюджет времени цикла |

Все timeout должны быть ненулевыми, а lock timeout — меньше statement timeout.
Пул открывает одно основное и не более 20 per-database подключений в порядке
имён баз. Закрытые подключения переоткрываются; непокрытые базы и пропуски
попадают в данные о coverage. Подробности: [`docs/connection-and-multidb.md`](../../docs/connection-and-multidb.md).

## Ограничения cardinality и хранения

| Переменная | Дефолт | Контракт |
| --- | ---: | --- |
| `KRONIKA_PG_MAX_TABLES` | `500` | Top N на ось выбора и базу. |
| `KRONIKA_PG_MAX_INDEXES` | `500` | Top N индексов на ось и базу. |
| `KRONIKA_PG_MAX_STATEMENTS` | `500` | Top N запросов на ось. |
| `KRONIKA_PG_MAX_LOCK_ROWS` | `1000` | Предел waiters, edges и nodes в lock-секции. |
| `KRONIKA_PG_MAX_PLANS` | `500` | Предел строк планов за чтение. |
| `KRONIKA_PG_MAX_PLAN_TEXT` | `32768` | Текст одного плана; допустимо 1–65536 байт. |
| `KRONIKA_PG_PLAN_TEXT_BUDGET` | `8388608` | Общий бюджет текста планов; `0` выключает текст, максимум 16 MiB. |
| `KRONIKA_PG_PLANS_INTERVAL_S` | `300` | Минимальный период `pg_store_plans`. |
| `KRONIKA_OS_MAX_DISKS` | `256` | Число младших устройств по `(major, minor)`. |
| `KRONIKA_OS_MAX_PROCS` | `4096` | Число младших числовых PID. |
| `KRONIKA_OS_MAX_CGROUPS` | `1024` | Число cgroup за обход. |
| `KRONIKA_OS_MAX_CGROUP_IO_ROWS` | `4096` | Строки cgroup I/O за проход. |
| `KRONIKA_OS_CGROUP_MAX_DEPTH` | `8` | Глубина дерева cgroup. |
| `KRONIKA_SEGMENT_MAX_BYTES` | `67108864` | Ротация по сырым байтам журнала; `0` запечатывает каждое окно. |
| `KRONIKA_SEGMENT_MAX_AGE_S` | `900` | Предельный возраст открытого сегмента. |
| `KRONIKA_JOURNAL_MAX_BYTES` | `1073741824` | Жёсткий предел журнала; при достижении выполняется досрочное запечатывание. |

Настройки, способные нарушить предел секции или словаря, отклоняются до начала
сбора. Ошибка разбора OS cap заменяет значение документированным дефолтом и
пишет warning.

## Расписание

`KRONIKA_INTERVAL_S` задаёт шаг таймера (`5` секунд). Значение `0` оставляет
только запуск по сигналу. Базовые интервалы источников:

| Источник | Переменная | Секунды |
| --- | --- | ---: |
| Activity | `KRONIKA_PG_ACTIVITY_INTERVAL_S` | 5 |
| Database | `KRONIKA_PG_DATABASE_INTERVAL_S` | 10 |
| Bgwriter/checkpointer | `KRONIKA_PG_BGWRITER_INTERVAL_S` | 10 |
| WAL | `KRONIKA_PG_WAL_INTERVAL_S` | 10 |
| PostgreSQL I/O | `KRONIKA_PG_IO_INTERVAL_S` | 10 |
| Статистика archiver | `KRONIKA_PG_ARCHIVER_INTERVAL_S` | 30 |
| Prepared transactions | `KRONIKA_PG_PREPARED_INTERVAL_S` | 30 |
| Vacuum progress | `KRONIKA_PG_PROGRESS_VACUUM_INTERVAL_S` | 10 |
| Statements | `KRONIKA_PG_STATEMENTS_INTERVAL_S` | 30 |
| User tables | `KRONIKA_PG_TABLES_INTERVAL_S` | 30 |
| User indexes | `KRONIKA_PG_INDEXES_INTERVAL_S` | 60 |
| Replication | `KRONIKA_PG_REPLICATION_INTERVAL_S` | 30 |
| Reset metadata | `KRONIKA_PG_RESET_METADATA_INTERVAL_S` | 30 |
| Instance metadata | `KRONIKA_INSTANCE_INTERVAL_S` | 60 |
| PostgreSQL settings | `KRONIKA_PG_SETTINGS_INTERVAL_S` | 3600 |
| Core OS | `KRONIKA_OS_CORE_INTERVAL_S` | 10 |
| Mount/topology | `KRONIKA_OS_MOUNTTOPO_INTERVAL_S` | 60 |
| Processes | `KRONIKA_OS_PROCESS_INTERVAL_S` | 5 |
| Process status | `KRONIKA_OS_PROCESS_STATUS_INTERVAL_S` | 30 |
| Cgroup | `KRONIKA_OS_CGROUP_INTERVAL_S` | 10 |
| Cgroup mapping | `KRONIKA_OS_CGROUP_MAPPING_INTERVAL_S` | 30 |
| PostgreSQL log | `KRONIKA_PG_LOG_INTERVAL_S` | 5 |

Activity ускоряется до `KRONIKA_PG_ACTIVITY_FAST_INTERVAL_S` (`1`), когда
число активных client backends достигает `KRONIKA_PG_ASH_ACTIVE_THRESHOLD`
(`20`). Репликация ускоряется до `KRONIKA_PG_REPLICATION_FAST_INTERVAL_S`
(`10`), когда lag достигает `KRONIKA_PG_REPL_LAG_TRIGGER_S` (`10`) или
удержанный WAL — `KRONIKA_PG_SLOT_RETAINED_TRIGGER_BYTES` (`1073741824`). Fast
interval не меньше базового выключает соответствующий триггер.

`SIGUSR2` принудительно читает все источники и запечатывает окно. `SIGTERM` и
`SIGINT` завершают цикл; уже синхронизированные кадры остаются в журнале и
запечатываются при следующем запуске.

## Необязательный источник журналов PostgreSQL

| Переменная | Дефолт | Назначение |
| --- | ---: | --- |
| `KRONIKA_PG_LOG_ENABLED` | true только при `KRONIKA_LOG_PATH` | Включить источник. |
| `KRONIKA_LOG_PATH` | не задан | Явный текущий log-файл. |
| `KRONIKA_LOG_ROOT` | не задан | Корень автоматического поиска журнала. |
| `KRONIKA_LOG_FORMAT` | `stderr` | `stderr` парсится; `csvlog` принимается, но отмечается как unsupported. |
| `KRONIKA_LOG_STATE_PATH` | `<out>/pg_log_tail.state` | Сохранённая позиция tail. |
| `KRONIKA_LOG_START_AT_BEGINNING` | `false` | Начать новый файл с нулевого offset. |
| `KRONIKA_LOG_DISCOVERY_INTERVAL_S` | `60` | Период повторного поиска. |

У tailer есть фиксированные пределы строк, байт, времени, backlog и выходных
событий. Ротация, усечение, бинарные строки, пропуск backlog и исчерпание
бюджета превращаются в typed gap rows: частичное чтение не выдаётся за полное.

## Fixture overrides Linux

`KRONIKA_PROC_ROOT`, `KRONIKA_SYS_ROOT` и `KRONIKA_STATVFS_FIXTURE` нужны для
BDD и parser fixtures. В production их обычно не задают.

## Канонический запуск

```sh
KRONIKA_PG_DSN='host=127.0.0.1 dbname=postgres user=kronika password=change-me' \
KRONIKA_OUT_DIR=/var/lib/pg_kronika \
KRONIKA_SOURCE_ID=1 \
pg_kronika-collector
```

У бинарника нет CLI-флагов и конфигурационного файла: environment variables —
полный операторский интерфейс.
