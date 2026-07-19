# Подключения коллектора к PostgreSQL

`pg_kronika-collector` обслуживает один экземпляр PostgreSQL. Он держит
основное подключение для instance-wide источников и до 20 подключений к
отдельным базам для database-local статистики.

## DSN и session contract

`KRONIKA_PG_DSN` принимает URI или `key=value` в синтаксисе
`tokio-postgres`. Код разбирает DSN в `tokio_postgres::Config`, после чего
структурно задаёт базу для per-database подключения и
`application_name=pg_kronika-collector/<version>`.

Для каждого подключения действуют:

| Настройка | Дефолт |
| --- | ---: |
| connect timeout | 5 s |
| TCP keepalive idle / interval / retries | 30 s / 10 s / 3 |
| `statement_timeout` | 15 000 ms |
| `lock_timeout` | 1 000 ms |
| `idle_in_transaction_session_timeout` | 10 000 ms |

Три PostgreSQL timeout настраиваются через
`KRONIKA_PG_STATEMENT_TIMEOUT_MS`, `KRONIKA_PG_LOCK_TIMEOUT_MS` и
`KRONIKA_PG_IDLE_IN_TX_TIMEOUT_MS`. Ноль запрещён, а `lock_timeout` должен быть
меньше `statement_timeout`.

TLS определяется DSN и возможностями `tokio-postgres` в текущей сборке;
коллектор вызывает `NoTls`, поэтому встроенного TLS connector сейчас нет.
Для удалённого подключения используйте доверенную сеть или защищённый tunnel.

## Основное подключение

Через main client читаются instance-wide представления: activity, database,
bgwriter/checkpointer, WAL, I/O, archiver, prepared transactions, vacuum
progress, replication, locks, reset metadata, settings и timestamp окна.

Перед циклом `ensure_main()` проверяет, не закрыт ли client. После reconnect
major заново берётся из handshake `server_version`; соединение с другим major
не продолжает использовать старую layout decision.

Ошибка восстановления main connection пропускает PostgreSQL-часть цикла и
пишется в log. Если log source наступил по расписанию, collector всё ещё может
записать log-only window.

## Перечисление баз

Pool refresh выполняет запрос:

```sql
SELECT datname
FROM pg_catalog.pg_database
WHERE datallowconn
  AND NOT datistemplate
  AND pg_catalog.has_database_privilege(datname, 'CONNECT')
ORDER BY datname
```

После SQL отбрасываются имена из `KRONIKA_PG_EXCLUDE_DATABASES` (разделитель
`;`). Первые 20 баз в порядке имени получают per-database connection. Остальные
остаются в `uncovered`; cap не настраивается через environment.

`KRONIKA_PG_POOL_REFRESH_SECS` (`600`) задаёт период refresh, если pool уже не
пуст. Закрытые clients удаляются и открываются снова. Ошибка подключения к
одной базе не отменяет остальные, но её имя остаётся uncovered до успешного
refresh.

База из DSN — только начальная точка для main connection. Режима «собирать
только `dbname` из DSN» нет.

## Database-local источники и coverage

Per-database clients читают `pg_stat_user_tables` и
`pg_stat_user_indexes`. `pg_stat_statements` и `pg_store_plans` —
instance-wide данные расширений, но SQL objects существуют только в базе
установки; collector ищет такую базу среди main и pool connections и кэширует
выбранный client.

Тяжёлые relation-size queries начинают со `statement_timeout=15000`. После
SQLSTATE `57014` timeout удваивается до
`KRONIKA_PG_HEAVY_TIMEOUT_CAP_MS` (`60000`) и запрос к той же базе повторяется.
SQLSTATE `55P03` считается lock contention и не расширяет timeout.

Ошибки одной database-local выборки не теряют весь сегмент:

- timeout увеличивает `timeouts`;
- SQLSTATE `42501` увеличивает `permission_skips`;
- lock conflict и прочие query failures учитываются отдельно;
- total становится unknown, если полное исходное множество неизвестно;
- collected/total и причина попадают в `collection_coverage`.

Sized sources выполняются под общим `KRONIKA_CYCLE_DB_BUDGET_MS` (`15000`)
в порядке statements, tables, indexes. Не вместившийся источник переносится на
следующий tick и фиксируется в log, а не считается пустым.

## Права

Минимальный login должен иметь `CONNECT` к нужным базам. Для полных данных
других ролей обычно нужен `pg_monitor` или эквивалентный набор прав, например
`pg_read_all_stats`. Недоступные поля PostgreSQL могут стать `NULL`, а
database-local permission failure попадёт в coverage.

Расширения необязательны:

- `pg_stat_statements` собирается из одной базы, где extension установлен;
- `pg_store_plans` поддерживает обнаруживаемые сигнатуры vadv и ossc forks;
- отсутствующий extension не останавливает core collection;
- `track_io_timing`, `track_wal_io_timing`, `compute_query_id` и extension
  versions пишутся в reset metadata и используются collection gates.

Коллектор маркирует SQL комментарием с версией и source file. Запросы видны в
`pg_stat_activity`, server log и statement instrumentation как работа
PgKronika.

## Конфиденциальность DSN

DSN передаётся через environment и может содержать password. Ограничьте доступ
к окружению процесса и service definition. Ни collector logs, ни segment
announcements намеренно не печатают DSN, но его хранение и rotation остаются
за оператором.

Полный список environment variables находится в
[`bins/pg_kronika-collector/README.ru.md`](../bins/pg_kronika-collector/README.ru.md).
