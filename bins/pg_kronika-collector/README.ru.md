# pg_kronika-collector

[English version](README.md)

`pg_kronika-collector` — единственный процесс, который подключается к
PostgreSQL и пишет PGM. Он читает наступившие по расписанию источники
PostgreSQL, Linux, cgroup и журнал stderr PostgreSQL, добавляет одно ограниченное
окно в `active.parts`, затем финализирует журнал в `<first_timestamp>.pgm` при
наступлении условия ротации.

Состояния `ready` и `sealed ...` идут в stdout, структурированные logfmt-события
— в stderr. Ошибка цикла сбора записывается в журнал, после чего демон повторяет
попытку. Ошибка конфигурации, первого подключения или открытия журнала
завершает процесс.

## Обязательные настройки

| Переменная | По умолчанию | Назначение |
| --- | ---: | --- |
| `KRONIKA_PG_DSN` | обязательна | URI или строка `key=value` для `tokio-postgres`. |
| `KRONIKA_OUT_DIR` | обязательна | Каталог с `active.parts` и готовыми `.pgm`. |
| `KRONIKA_SOURCE_ID` | `0` | `u64` в каталоге сегмента. Для нескольких коллекторов в общем каталоге задайте разные ненулевые значения. |
| `KRONIKA_LOG_LEVEL` | `info` | `error`, `warn`, `info`, `debug` или `trace`; неверное значение заменяется на `info`. |

Отсутствующий выходной каталог создаётся. Права файлов зависят от umask
процесса. В сегменты могут попасть SQL, планы, аргументы процессов и текст
журнала — ограничьте доступ к каталогу.

## Подключения и ограничения запросов

| Переменная | По умолчанию |
| --- | ---: |
| `KRONIKA_PG_STATEMENT_TIMEOUT_MS` | `15000` |
| `KRONIKA_PG_LOCK_TIMEOUT_MS` | `1000` |
| `KRONIKA_PG_IDLE_IN_TX_TIMEOUT_MS` | `10000` |
| `KRONIKA_PG_EXCLUDE_DATABASES` | пусто; имена разделяются `;` |
| `KRONIKA_PG_POOL_REFRESH_SECS` | `600` |
| `KRONIKA_PG_HEAVY_TIMEOUT_CAP_MS` | `60000` |
| `KRONIKA_CYCLE_DB_BUDGET_MS` | `15000`; `0` отключает бюджет времени цикла |

Все тайм-ауты обязаны быть ненулевыми, а lock timeout — меньше statement
timeout. Пул открывает одно основное подключение и не более 20 — по одному на
базу, в алфавитном порядке имён. Закрытые подключения переоткрываются; базы без
покрытия и пропуски попадают в данные о покрытии. Подробности:
[`docs/connection-and-multidb.md`](../../docs/connection-and-multidb.md).

## Ограничения кардинальности и хранения

| Переменная | По умолчанию | Контракт |
| --- | ---: | --- |
| `KRONIKA_PG_MAX_TABLES` | `500` | Не более N таблиц на измерение и базу. |
| `KRONIKA_PG_MAX_INDEXES` | `500` | Не более N индексов на измерение и базу. |
| `KRONIKA_PG_MAX_STATEMENTS` | `500` | Не более N запросов на измерение. |
| `KRONIKA_PG_MAX_LOCK_ROWS` | `1000` | Предел waiters, edges и nodes в lock-секции. |
| `KRONIKA_PG_MAX_PLANS` | `500` | Предел строк планов за чтение. |
| `KRONIKA_PG_MAX_PLAN_TEXT` | `32768` | Текст одного плана; допустимо 1–65536 байт. |
| `KRONIKA_PG_PLAN_TEXT_BUDGET` | `8388608` | Общий бюджет текста планов; `0` отключает текст, максимум 16 МиБ. |
| `KRONIKA_PG_PLANS_INTERVAL_S` | `300` | Минимальный период `pg_store_plans`. |
| `KRONIKA_OS_MAX_DISKS` | `256` | Число младших устройств по `(major, minor)`. |
| `KRONIKA_OS_MAX_PROCS` | `4096` | Число младших числовых PID. |
| `KRONIKA_OS_MAX_CGROUPS` | `1024` | Число cgroup за обход. |
| `KRONIKA_OS_MAX_CGROUP_IO_ROWS` | `4096` | Строки cgroup I/O за проход. |
| `KRONIKA_OS_CGROUP_MAX_DEPTH` | `8` | Глубина дерева cgroup. |
| `KRONIKA_SEGMENT_MAX_BYTES` | `67108864` | Ротация по сырым байтам журнала; `0` финализирует каждое окно. |
| `KRONIKA_SEGMENT_MAX_AGE_S` | `900` | Предельный возраст открытого сегмента. |
| `KRONIKA_JOURNAL_MAX_BYTES` | `1073741824` | Жёсткий предел журнала; при достижении — досрочная финализация. |

Настройки, способные нарушить предел секции или словаря, отклоняются до начала
сбора. Ошибка разбора OS cap заменяет значение документированным умолчанием и
пишет warning.

## Расписание

`KRONIKA_INTERVAL_S` задаёт такт таймера (`5` секунд). Значение `0` оставляет
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
| Базовая ОС | `KRONIKA_OS_CORE_INTERVAL_S` | 10 |
| Mount/topology | `KRONIKA_OS_MOUNTTOPO_INTERVAL_S` | 60 |
| Processes | `KRONIKA_OS_PROCESS_INTERVAL_S` | 5 |
| Process status | `KRONIKA_OS_PROCESS_STATUS_INTERVAL_S` | 30 |
| Cgroup | `KRONIKA_OS_CGROUP_INTERVAL_S` | 10 |
| Cgroup mapping | `KRONIKA_OS_CGROUP_MAPPING_INTERVAL_S` | 30 |
| Журнал PostgreSQL | `KRONIKA_PG_LOG_INTERVAL_S` | 5 |

Каждое фактическое чтение `pg_store_plans` добавляет согласованную строку
reset-метаданных с точным `ts` снимка планов. Коллектор проверяет reset-состояние
до и после чтения и отбрасывает снимок планов, если состояние изменилось или
его не удалось прочитать.

Activity ускоряется до `KRONIKA_PG_ACTIVITY_FAST_INTERVAL_S` (`1`), когда число
активных client backends достигает `KRONIKA_PG_ASH_ACTIVE_THRESHOLD` (`20`).
Репликация ускоряется до `KRONIKA_PG_REPLICATION_FAST_INTERVAL_S` (`10`), когда
lag достигает `KRONIKA_PG_REPL_LAG_TRIGGER_S` (`10`) или задержанный WAL —
`KRONIKA_PG_SLOT_RETAINED_TRIGGER_BYTES` (`1073741824`). Если ускоренный
интервал не короче базового, соответствующий триггер отключается.

`SIGUSR2` принудительно читает все источники и финализирует окно. `SIGTERM` и
`SIGINT` завершают цикл; уже синхронизированные кадры остаются в журнале и
финализируются при следующем запуске.

## Источник журнала PostgreSQL

Сбор журнала PostgreSQL включён по умолчанию. Если `KRONIKA_LOG_PATH` не задан,
при каждой попытке поиска коллектор проверяет, что `SHOW log_destination`
содержит `stderr`, затем вызывает
`pg_catalog.pg_current_logfile('stderr')`. Относительный путь разрешается
относительно `SHOW data_directory` либо `KRONIKA_LOG_ROOT`, если задано это
переопределение.

Результат записывается в `pg_log_source_status`:

| `state` | Значение |
| --- | --- |
| `collecting` | Поддерживаемый файл открыт и обработан. Читаемый файл без новых строк даёт то же состояние: отсутствие событий не считается ошибкой чтения. |
| `collecting_degraded` | Последний известный файл обработан, но поиск пути не удалось обновить: не было подключения к PostgreSQL либо запрос поиска завершился ошибкой. Чтение состоялось; само это состояние не доказывает потерю данных. |
| `unavailable` | Поддерживаемый файл прочитать не удалось. Поле `reason` различает `no_current_logfile`, `unsupported_format`, `missing_file`, `permission_denied`, `read_error` и ошибку поиска при отсутствии известного файла. |
| `disabled` | Оператор явно задал `KRONIKA_PG_LOG_ENABLED=0`. |

Строка состояния записывается при первом наблюдении, при изменении состояния,
причины, парсера или пути, а также по истечении интервала без изменений.
Последняя строка доступна в объекте `pg_log` ответа `GET /v1/sources`.

Коллектор не меняет настройки PostgreSQL и права доступа к файлам. Если
сохранённой позиции чтения ещё нет, первое чтение нового файла начинается с
конца. Значение `KRONIKA_LOG_START_AT_BEGINNING=1` начинает чтение с нулевого
смещения.

При смене найденного пути коллектор ещё один раз читает предыдущий файл в
обычных пределах одного цикла: не более 4096 строк, 1 МБ и 50 мс. После
успешной записи этого результата он переключается на новый файл, даже если
старый хвост не исчерпан. Первый записанный цикл нового файла содержит
`pg_log_gap` с `reason=rotation`; если размер оставшегося хвоста удалось
определить, `bytes_skipped` показывает число непрочитанных байтов. Такая
политика ограничивает задержку свежих событий одной попыткой чтения старого
файла.

| Переменная | По умолчанию | Назначение |
| --- | ---: | --- |
| `KRONIKA_PG_LOG_ENABLED` | `true` | Искать и читать поддерживаемый файловый журнал; явное значение `false` отключает источник. |
| `KRONIKA_PG_LOG_INTERVAL_S` | `5` | Период попыток прочитать известный файл. |
| `KRONIKA_LOG_DISCOVERY_INTERVAL_S` | `60` | Период повторного поиска пути через PostgreSQL, в том числе пока источник не найден. |
| `KRONIKA_PG_LOG_STATUS_INTERVAL_S` | `300` | Период записи состояния без изменений; значение должно быть больше нуля. |
| `KRONIKA_LOG_PATH` | не задан | Заменить найденный путь явным; переопределение не отменяет явное отключение источника. |
| `KRONIKA_LOG_ROOT` | не задан | Корень автоматического поиска журнала. |
| `KRONIKA_LOG_FORMAT` | `stderr` | `stderr` разбирается; `csvlog` принимается, но получает состояние `unavailable` с причиной `unsupported_format`. |
| `KRONIKA_LOG_STATE_PATH` | `<out>/pg_log_tail.state` | Путь к сохранённой позиции чтения. |
| `KRONIKA_LOG_START_AT_BEGINNING` | `false` | Начать новый файл с нулевого смещения. |

Модуль чтения применяет фиксированные пределы строк, байтов, времени,
накопившихся данных и выходных событий. Ротация, усечение, бинарные строки,
пропуск накопившихся данных и исчерпание бюджета превращаются в типизированные
строки `pg_log_gap`: частичное чтение не выдаётся за полное.

## Фикстуры Linux (переопределение)

`KRONIKA_PROC_ROOT`, `KRONIKA_SYS_ROOT` и `KRONIKA_STATVFS_FIXTURE` нужны для
BDD и тестов парсера. В production их обычно не задают.

## Канонический запуск

```sh
KRONIKA_PG_DSN='host=127.0.0.1 dbname=postgres user=kronika password=change-me' \
KRONIKA_OUT_DIR=/var/lib/pg_kronika \
KRONIKA_SOURCE_ID=1 \
pg_kronika-collector
```

У бинарника нет CLI-флагов и конфигурационного файла: переменные окружения —
полный интерфейс оператора.
