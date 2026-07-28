# Расширение сбора: оставшиеся диагностические линзы

Дата: 2026-07-27. Статус и остаток работ сверены с `origin/main` на
`0bba2d02901b88792f35b801c2c9cc65bdcf5352`. Определяет состав и порядок
серии PR по добавлению ещё отсутствующих линз; каждый logical source
поставляется отдельным PR по действующему плейбуку one-PR-per-metric.

Общий междокументный порядок, зависимости и границы пересекающихся работ
задаёт
[`2026-07-28-diagnostics-roadmap-design.md`](2026-07-28-diagnostics-roadmap-design.md).

## Что ещё предстоит

На current main ни один активный `T*` ID не закрыт целиком. Два пункта
частично реализованы, и ниже для них оставлен только остаток:

| Статус | Количество | IDs |
| --- | ---: | --- |
| Частично реализовано | 2 | `T1-5`, `T3-3` |
| Будущее | 24 | `T1-1`, `T1-2`, `T1-3`, `T1-4`, `T1-6`, `T1-7`, `T1-8`, `T1-9`; `T2-1`, `T2-2`, `T2-3`, `T2-4`, `T2-5`, `T2-6`, `T2-7`, `T2-8`, `T2-9`, `T2-10`, `T2-11`; `T3-1`, `T3-2`, `T3-4`, `T3-5`, `T3-6` |
| Реализовано среди активных IDs | 0 | — |
| Отклонено/заменено среди активных IDs | 0 | — |

Приоритет: сначала Tier 1, затем обнаруживаемые Tier 2; `T2-10` предшествует
extension-dependent источникам. Tier 3 поставляется после inventory
расширений, work bounds и production-path fixture. Общая dependency-очередь
с Health Score и product actions задана верхнеуровневой дорожной картой.

## Задача

PgKronika — диагностическая история («чёрный ящик») экземпляра PostgreSQL:
её ценность измеряется тем, на сколько инцидентных вопросов можно ответить
постфактум по уже записанным сегментам. Текущее покрытие широкое (полный
перечень секций — `docs/type-registry/postgresql.md` и
`docs/type-registry/os.md`), но инвентаризация инцидентных вопросов
показывает пробелы, из-за которых часть классических аварий по записи не
разбирается:

- «pg_wal съел диск» — рост занятости раздела виден, атрибуция к WAL, к
  очереди архивации и к позиции REDO — нет;
- «vacuum не чистил, таблица пухла» — из четырёх держателей
  xmin-горизонта записываются два (долгая транзакция, 2PC); `xmin` и
  `catalog_xmin` слотов и `backend_xmin` walsender'ов не пишутся;
- «на стендбае массово отменяются запросы» — есть суммарный счётчик
  `conflicts`, нет разбивки по причинам;
- «savepoint-шторм / multixact-давление» — SLRU-кэши не записываются,
  инцидент постфактум невосстановим;
- «логическое декодирование спиллит гигабайты» — каталожные слоты
  записываются, статистика spill/stream публикующей стороны — нет;
- «CREATE INDEX CONCURRENTLY стоит» / «долгий COPY» — прогресс пишется
  только для VACUUM;
- «кто генерит WAL и temp» — топы statements выбираются по времени и
  calls, генератор WAL может не попасть в выборку;
- дедлок записывается как сгруппированная ошибка без участников и их
  запросов.

## Как проводился анализ

Инвентаризация собственного покрытия — по registry и исходникам
источников (`crates/kronika-registry`, `crates/kronika-source-{pg,os,log}`).
Каждый кандидат связан с вопросом посмертного разбора, проверен на
возможность ограниченного безопасного сбора и сопоставлен с текущими
registry/source контрактами. Версии появления и изменения схем
представлений PostgreSQL сверены по release notes и официальной
документации 15–18 (<https://www.postgresql.org/docs/>).

## Не цели

- Этот каталог не определяет алертинг, Health Score и рекомендации: он
  фиксирует контракты сохраняемых фактов. Health Score как отдельная
  проекция этих фактов задан в
  [`2026-07-28-health-score-diagnostics-design.md`](2026-07-28-health-score-diagnostics-design.md);
  автоматическая remediation не входит ни в одну из программ.
- Метрики пулеров (PgBouncer, pgpool, Odyssey): это не PostgreSQL —
  другой протокол и admin-консоль; co-located процесс пулера и его
  соединения видны существующими OS-линзами.
- `pg_stat_ssl` / `pg_stat_gssapi`: TLS/GSSAPI-инциденты всплывают как
  ошибки аутентификации в connection-линзе (T2-4) и в сгруппированных
  ошибках лога; отдельная линза не окупается.
- Полный скан `pg_buffercache` построчно (миллионы строк на больших
  shared_buffers) — записываются только агрегаты.
- Точный bloat через `pgstattuple` полным сканом — стоимость чтения всей
  таблицы несовместима с фоновым сбором; допустима только SQL-оценка.
- Хэширование всего каталога для change detection — хронология DDL
  решается лог-линзой (см. T2-5).
- Удалённый сбор с managed-сервисов (RDS, Cloud SQL): контракт проекта —
  коллектор на хосте БД; `pg_ls_*` и локальные joins там недоступны,
  такие конфигурации не поддерживаются.

## Сквозные контракты серии

Правила, общие для всех линз каталога; каждый PR серии выполняет их как
definition of done, а не решает заново.

**Права и деградации.** Базовый профиль — `pg_monitor`; всё, что требует
прав сверх него, называется в тексте линзы явно, и каждый PR добавляет
строку в таблицу прав и деградаций `postgresql-collection.md`. Тихая
пустота запрещена: недоступность источника (permission denied, таймаут,
NULL из-за отсутствия грантов) фиксируется существующими механизмами
provenance (`snapshot_coverage`, статус источника по образцу
`pg_log_source_status`). Справочно: `pg_ls_waldir`/`pg_ls_archive_statusdir`
и функции `pg_visibility` исполняемы под `pg_monitor` (через
`pg_read_all_stats` и `pg_stat_scan_tables` соответственно), `pg_aios`
читается под `pg_read_all_stats`; `pg_sequences.last_value` и `pg_stats`
требуют грантов на объекты (см. T2-7, T2-8).

**Reset и staleness.** Новый cumulative-источник несёт свой `stats_reset`
колонкой в строках секции там, где PostgreSQL его даёт per-row
(`pg_stat_slru` — сбрасывается по-кэшево, `pg_stat_subscription_stats` —
по-подписочно), синглтоны — через `reset_metadata`. Где reset-метки нет
(`pg_wait_sampling`, `pg_stat_kcache`, включая эвикцию записей kcache),
это документируется: уменьшение счётчика читается как Reset существующей
registry-семантикой, окно «reset с донабором за интервал» остаётся
слепым и признаётся.

**Версионирование по факту, не по мажору.** Возможности расширений
гейтятся по `extversion`/`to_regproc`, а не по версии сервера: после
pg_upgrade парк живёт со старыми версиями расширений (прецедент — шесть
layout'ов `pg_stat_statements`). Изменения схем каталожных вью внутри
матрицы 15–18 дают версионные layout'ы по действующему правилу реестра.

**Лог-линзы.** Контракт локали: шаблоны разбираются для en и ru (по
существующей конвенции парсера); на прочих локалях линза деградирует в
событие без структурного разбора, что видно в статусе источника.
Существующий parser уже сохраняет многострочные
DETAIL/HINT/CONTEXT/STATEMENT continuations. Новая линза выполняет
структурное извлечение до усечения текстовых полей и не дублирует этот
механизм.

**Управляемость и стоимость.** Дорогие и включаемые-по-обнаружению линзы
получают env-выключатель (контракт «интервал 0 = выключено»
документируется как общий); каждый PR приводит оценку байт/час на
референсной нагрузке — при включённой ротации новая секция не «доливает
диск», а укорачивает горизонт истории. Инцидентные ускорители (fast
paths) капируются: пик записи приходится на момент, когда диску хоста и
так плохо.

**Идентичность queryid.** Секции, несущие queryid (statements, T3-1,
T3-2), записывают его одним физическим типом, чтобы join на чтении не
требовал приведения; интерпретация — через уже записываемый
`compute_query_id`.

## Каталог линз

Формат: источник → разрез → семантика записи → частота → инцидентный
вопрос, на который отвечает запись. Семантики — существующие в registry:
snapshot_full, conditional_full, on_change, event_stream.

Составные `T1-4`, `T1-8` и `T2-2` являются coordination IDs: каждый
logical source или независимое расширение поставляется отдельным PR, который
называет parent ID и свою точную slice. Для `T1-1` source/registry предшествует
отдельному scheduler fast-path PR.

### Tier 1 — оставшееся консенсус-ядро

Закрывает наиболее важные невосстановимые постфактум классы аварий. Новых
зависимостей сверх уже используемых не добавляется: каталожные
представления и функции под `pg_monitor`; T1-5 — лог-линза поверх уже
тейлящегося stderr, T1-6 живёт в существующей линзе
`pg_stat_statements`.

**T1-1. `pg_wal_storage` — физика WAL и очередь архивации.**
Кластер, snapshot_full. Ответ на «pg_wal съел диск»: три горизонта
удержания — min(`restart_lsn`) слотов, очередь архивации и REDO последнего
чекпойнта (`pg_control_checkpoint()`). Новая секция связывает существующие
slot/archiver сигналы с ещё отсутствующими физическими фактами.

Механика фиксируется спекой, потому что наивная реализация опасна ровно
во время целевого инцидента (сотни тысяч файлов в `pg_wal` и
`archive_status`):

- число сегментов — без `stat()` каждого файла; байты — арифметикой
  `count × wal_segment_size` (GUC уже записывается), не суммой размеров;
- глубина очереди архивации — `count(*)` по `.ready` плюс возраст в
  байтах LSN-арифметикой по **имени** старейшего `.ready` (имена
  сегментов упорядочены по LSN), не по mtime; дешёвый второй сигнал —
  уже собираемые `last_archived_wal`/`last_failed_wal` архивера;
- вся агрегация — на стороне сервера, строки файлов в коллектор не
  тянутся; истечение `statement_timeout` — честная деградация с
  пометкой в coverage, не молчание;
- роль: на реплике `pg_current_wal_lsn()` выбрасывает ошибку — линза
  несёт флаг in_recovery и пишет `pg_last_wal_replay_lsn()` /
  `pg_last_wal_receive_lsn()`; переполнение `pg_wal` на стендбае
  (обрыв restore, `archive_mode=always`) — поддерживаемый сценарий;
- частота: база 60 с + триггерное ускорение до 10 с по уже собранным
  сигналам (очередь `.ready` растёт; положительная дельта размера
  `pg_wal` несколько циклов подряд) — в духе существующих fast paths.

**T1-2. `pg_stat_slru` — давление на SLRU-кэши.**
Все колонки + `stats_reset` per-row (сбрасывается по-кэшево), 8–9 строк
(PG13+). Кластер, snapshot_full, 30 с. Отвечает: «subtrans overflow от
savepoints при длинной транзакции», «multixact-шторм от FK» — классы
аварий, которые без записанной истории SLRU не разбираются вообще.
PG17 переименовал значения `name` (`Xact`→`transaction` и т.д.):
записываются как есть — честные данные; таблица соответствия имён — в
справочнике линзы, непрерывность рядов через апгрейд мажора решает
чтение.

**T1-3. `pg_stat_database_conflicts` — причины отмен на стендбае.**
Разбивка: tablespace, lock, snapshot, bufferpin, deadlock,
`confl_active_logicalslot` (PG16+ — версионный layout 15 / 16+). База,
snapshot_full, 30 с. Записывается **всегда**, не только в recovery:
строки существуют и на праймери, а последний всплеск отмен перед
промоутом — самое ценное окно форензики failover'а. Существующий суммарный
`pg_stat_database.conflicts` остаётся отдельным baseline-сигналом.

**T1-4. Остальные прогресс-представления: `pg_stat_progress_analyze`,
`_cluster`, `_create_index`, `_basebackup`, `_copy`.**
Процесс, conditional_full (строки только при активной операции), 10 с.
Версионные layout'ы внутри матрицы: `_copy` — PG17 добавил
`tuples_skipped`; `_analyze` — PG18 добавил `delay_time`. Отвечает:
«почему CREATE INDEX CONCURRENTLY стоит и кого ждёт» (`lockers_total`),
«сколько осталось COPY/базовому бэкапу».

**T1-5. `pg_log_deadlocks` — типизированное событие дедлока.**
**Осталось:** поверх уже сохраняемых многострочных DETAIL/STATEMENT создать
отдельную event_stream-линзу. Из `deadlock detected` и DETAIL до усечения
извлекаются участники, pid'ы, рёбра «кто кого ждал», запросы сторон и жертва.
Нужны en+ru structural templates и fallback при отсутствии `%p` в
`log_line_prefix`: событие без pid жертвы честнее эвристики. Существующий
счётчик `pg_stat_database.deadlocks` остаётся агрегатным baseline.

**T1-6. Расширение выборки кандидатов `pg_stat_statements`: топ по
`wal_bytes` и по `temp_blks_written`.**
Не новая линза — остаётся добавить два плеча отбора кандидатов поверх уже
сохраняемых полей. Они отвечают: «какой запрос генерит WAL» и «кто пишет
temp» — редкий генератор может не попасть в текущие top-N времени/calls.
Реализация — один
материализованный скан `pg_stat_statements` и четыре top-N поверх него
(каждое UNION-плечо в лоб — отдельная материализация SRF, их станет
не две, а четыре); ось `wal_bytes` гейтится по extversion (pgss 1.8+);
суммарный потолок кандидатов проверяется против лимита строк секции.

**T1-7. Держатели xmin-горизонта — колонки в существующих линзах.**
`xmin` и `catalog_xmin` в `pg_replication_slots` (`1_017`),
`backend_xmin` walsender'а в `pg_stat_replication` (`1_016`). Они закрывают
недостающие slot/walsender стороны вопроса «кто держал горизонт».

**T1-8. Мелкие колонки и оси в существующих линзах.**
- `pg_stat_user_indexes`: добавить гарантированную ось `NOT indisvalid`,
  чтобы небольшой брошенный CIC-индекс не терялся за текущими top-N;
- `pg_storage_mount`: inode-счётчики из statvfs, nullable — на btrfs
  `f_files = 0`, ноль не означает «inode кончились»;
- `pg_stat_user_tables`: `relpersistence` — после crash unlogged-таблицы
  усечены, ретроспективе нужен их инвентарь на момент до падения;
- кластерный синглтон: `pg_notification_queue_usage()` — заполнение
  очереди NOTIFY (0..1); «слушатель умер, очередь дошла до предела» без
  записи не разбирается.

**T1-9. `pg_stat_recovery_prefetch` — эффективность replay на стендбае.**
Синглтон (PG15+), snapshot_full, 30 с, только в recovery; `stats_reset`
— через reset_metadata. Отвечает: «replay lag упирается в I/O или в
одиночный воспроизводящий процесс» (`io_depth`, `wal_distance`,
skip-счётчики). Startup process частично виден в activity по wait events;
пункт включён за дешевизну и стоит в хвосте тира.

### Tier 2 — оставшееся по обнаружению объекта или включённой настройки

**T2-1. `pg_stat_replication_slots` — spill/stream логического
декодирования (PG14+).**
Слот, snapshot_full, 30 с; строки только при логических слотах.
`spill_txns`/`spill_bytes`/`spill_count`, `stream_*`, `total_txns`/
`total_bytes`, `stats_reset` per-row. Отвечает: «wal sender жрёт CPU и
диск, `pg_replslot` разросся» — публикующая сторона, где диск и умирает;
T2-2 закрывает подписчика, эта линза — источник. Каталожный `1_017`
статистики не содержит.

**T2-2. Логическая репликация на стороне подписчика:
`pg_stat_subscription` + `pg_stat_subscription_stats`.**
Подписка, snapshot_full, 30 с; строки только при подписках. Layout'ы
внутри матрицы: `pg_stat_subscription` — `worker_type` с PG17;
`pg_stat_subscription_stats` (PG15+) — семь счётчиков конфликтов
`confl_*` с PG18; `stats_reset` per-row.

**T2-3. `pg_buffercache_summary()` + `pg_buffercache_usage_counts()` —
температура кэша.**
Кластер, snapshot_full, 60 с. Ворота — `extversion ≥ 1.4` (через
`to_regproc`), не «PG16+»: после pg_upgrade расширение живёт старой
версией без этих функций. Обе функции читают заголовки буферов без
полного скана представления (см. «Не цели»). Отвечает: «сколько dirty,
насколько прогрет кэш до/после инцидента».

**T2-4. `pg_log_connections` — connection-события из лога.**
Лог-линза, event_stream с агрегацией по интервалу: authorized,
authentication failed, disconnection, too many connections. Требует
`log_connections`/`log_disconnections`; в PG18 GUC стал списком стадий с
новыми формами сообщений — парсер знает оба формата. Оговорка стоимости:
на хостах с высоким connection churn включение GUC кратно раздувает
stderr, а капы тейлера общие — шторм соединений может выбивать
backlog-гэпами другие лог-линзы; это документируется в справочнике
линзы. Отвечает: «connection storm — кто и откуда ломился».

**T2-5. `pg_log_ddl` — хронология DDL.**
Лог-линза, event_stream: statement-строки DDL с базой, ролью и текстом.
Требует `log_statement=ddl` (или mod/all). **Обязательный контракт —
редакция секретов до записи**: `CREATE/ALTER ROLE|USER ... PASSWORD` и
password в OPTIONS user mapping маскируются на парсере — сегменты
иммутабельны, из них не вычистить то, что легко затереть в логфайле.
Тексты идут в словарь с потолком длины и группировочным капом (миграция
на 10k партиций — это 10k уникальных строк за интервал). Наследуемые
слепые зоны самого `log_statement` документируются: DDL внутри
функций/DO-блоков не логируется, extended protocol даёт префикс
`execute` вместо `statement`. Отвечает: «что меняли в схеме перед
инцидентом».

**T2-6. Лог-линза сбоя архивации.**
`archive command failed` + stderr архив-команды из лога: причина отказа
(нет места, auth, сеть), которой нет в существующих archiver counters.
Закрывает вторую половину вопроса T1-1 «архивация стоит — почему».

**T2-7. `pg_sequence_health` — запас последовательностей.**
`pg_sequences`: остаток до предела, топ-N худших; предел считается по
min(maxvalue, максимум типа owned-колонки) — int8-последовательность,
кормящая int4-колонку, переполняется задолго до maxvalue. База,
snapshot_full, 3600 с. Права:
`last_value` виден только при USAGE/SELECT на последовательность,
`pg_monitor` их не даёт — требование грантов фиксируется по сквозному
контракту, NULL не трактуется как запас. Отвечает: «последовательность
исчерпалась» — редкая, но мгновенно фатальная авария.

**T2-8. `pg_table_bloat_estimate` — SQL-оценка блоата.**
Классическая оценка по статистике (без pgstattuple), топ-N таблиц, база,
snapshot_full, 3600 с. Права: оценка опирается на `pg_stats`
(security barrier — строки видны только при SELECT на таблицу);
под чистым `pg_monitor` нужен `pg_read_all_data` (PG14+) или гранты —
фиксируется по сквозному контракту, пустой `pg_stats` виден в coverage.
Документируемые слепые зоны: TOAST-блоат оценка не видит (пухнущий
toast при стабильном heap), нестандартные типы искажают оценку; на
каталогах в сотни тысяч отношений действует heavy cap и per-db бюджет
цикла.

**T2-9. Детализация prepared transactions: gid и owner.**
Остаётся отдельный per-transaction contract: `pg_prepared_xacts` построчно
(gid, owner, database, prepared_at), conditional_full — строки только при
наличии 2PC; максимум `max_prepared_transactions` строк. Он дополняет
существующий aggregate: gid и owner дают указатель на приложение/transaction
manager, которого в агрегате нет.

**T2-10. Инвентарь расширений — `pg_extension` on_change.**
Имя и версия по базам. Помимо «обновляли ли расширение между
сегментами», это базис интерпретации Tier 3: счётчики kcache и
wait_sampling трактуются через историю extversion.

**T2-11. `db_size_approx` — дешёвая оценка размера базы.**
Добавить к существующей database-линзе оценку по `pg_class.relpages` с
явной семантикой approximation и coverage по базам. `pg_database_size()` не
используется: обход большого числа файлов не соответствует фоновому бюджету.

### Tier 3 — оставшаяся продвинутая форензика, по обнаружению

**T3-1. `pg_wait_sampling` — профиль ожиданий.**
При обнаруженном расширении. Дизайн зафиксирован спекой, потому что
наивное «дельты профиля на каждом тике activity» — авария по объёму:
чтение `pg_wait_sampling_profile` сериализует весь профиль через
shm_mq коллектор-воркера, при дефолтном `profile_pid=true` профиль
ключуется по pid и растёт неограниченно (мёртвые pid'ы живут до reset).
Контракт: серверная агрегация `GROUP BY event_type, event, queryid`
(pid схлопывается), top-N по count с пометкой усечения в coverage,
собственный интервал 10 с (не тик activity, включая fast path),
рекомендация `profile_pid = false` в справочнике линзы. Отсутствие
reset-метки документируется по сквозному контракту.

**T3-2. `pg_stat_kcache` — реальные ресурсы ОС по запросам.**
При обнаруженном расширении: user/system CPU, физические reads/writes,
page faults, context switches per queryid. Даёт измеренную декомпозицию
времени запроса (CPU против диска) вместо косвенного вычитания
blk-времён из total_time. Версионируется по версии расширения
(2.2+ разделил exec/plan-счётчики — сразу два layout'а); эвикция
записей kcache читается как Reset и документируется.

**T3-3. `pg_log_explain_plans` — планы auto_explain из лога.**
**Осталось:** распознавать auto_explain в уже тейлящемся stderr, выделять
границы плана, создавать `pg_log_explain_plans` и сохранять payload через
существующий bounded/deduplicated `dict.blobs`. Ротация посреди плана даёт
событие «план усечён», не тихий мусор; queryid извлекается при
`auto_explain.log_verbose`. Нужны source/registry wiring и BDD для границ,
усечения и queryid.

**T3-4. PG18: per-backend I/O и WAL.**
`pg_stat_get_backend_io()` / `pg_stat_get_backend_wal()` — атрибуция
I/O и WAL конкретному процессу. Снимается только при сработавшем fast
path и с капами: top-K бэкендов по активности из уже собранного
activity-снимка, только ненулевые строки. Известное ограничение
фиксируется в справочнике: бэкенд публикует свою статистику в shared
memory на границах транзакций (`pgstat_report_stat`) — у долгого
запроса-виновника числа отстают до завершения; разрешение линзы — «по
границам транзакций», не секунда.

**T3-5. PG18: `pg_aios` — снимок asynchronous I/O в полёте.**
conditional_full при io-давлении (существующий триггер ускорения или
PSI io); права — `pg_read_all_stats`; кардинальность ограничена
`io_max_concurrency × backends`. Форензика зависаний AIO-подсистемы.

**T3-6. `pg_visibility_map_summary()` — доля all-visible/all-frozen.**
Для top-N таблиц из сохранённого freeze-horizon baseline, 3600 с, при
установленном contrib pg_visibility; доступ — `pg_stat_scan_tables`
(входит в `pg_monitor`), читаются только VM-форки. Отвечает: «почему
VACUUM не двигает frozen horizon — сколько реально осталось заморозить».

## Порядок поставки

Каждая линза — отдельный PR по плейбуку (registry-секция, источник,
проверка, README); дополнительно к плейбуку каждый PR серии закрывает
сквозные контракты: строка прав/деградаций, учёт reset, оценка байт/час,
стратегия проверки. Общий порядок относительно Health Score, catalog
sources и product API задаёт верхнеуровневая дорожная карта; порядок ниже
остаётся локальной очередью независимых линз сбора.

Стратегии проверки: live-BDD (текущая матрица), golden (кодеки),
standby-BDD (требует реплику в матрице — её сегодня нет). Линзы T1-1
(recovery-ветка), T1-3, T1-9 живым BDD без реплики не проверяются;
решение «поднимать ли standby в BDD-матрице» принимается один раз перед
их PR, до тех пор они закрываются golden'ом. T2-2 требует
publisher/subscriber, T3-1/T3-2 — расширений в Nix-образе.

Порядок:

1. Tier 1: T1-1 (`pg_wal_storage`) → T1-6 (топы WAL/temp) →
   T1-5 (deadlock) → T1-2 (SLRU) → T1-3 (conflicts) → T1-4 (пять новых
   progress sources) → T1-7 (xmin-квадрант) → T1-8 (колонки) →
   T1-9 (prefetch).
2. Tier 2: T2-10 → T2-1 → T2-2 → T2-3 → T2-4 → T2-5 → T2-6 →
   T2-7 → T2-8 → T2-9 → T2-11.
3. Tier 3 — после T2-10 и по мере появления потребности; T3-4/T3-5 после появления
   PG18 в проде у первых пользователей.

Tier 1 не требует новых зависимостей; изменения конфигурации PostgreSQL
нужны только лог-линзам Tier 2/3 (`log_connections`, `log_statement`,
auto_explain) — рекомендуемый GUC-baseline с указанием restart/reload
приводится в справочнике первой из них. Tier 2/3 деградируют в пустые
результаты только после полного успешного attempt; иначе сохраняют typed
`partial`, `not_collected`, `unavailable` или `not_applicable` с coverage,
когда объект, настройка или права не обнаружены.

## Реализовано на current main

Baseline ниже не входит в активную очередь и не повторяется в будущих PR.

| ID | Возможность | Production evidence | Test/BDD/docs evidence |
| --- | --- | --- | --- |
| `BASE-M01` | Суммарные database conflicts | `crates/kronika-registry/src/codec/pg_stat_database.rs`, `crates/kronika-source-pg/src/database.rs` | `crates/kronika-bdd/features/pg_stat_database.feature`, `docs/type-registry/postgresql.md` |
| `BASE-M02` | Vacuum progress, включая PG18 `delay_time` | `crates/kronika-source-pg/src/progress_vacuum.rs`, `crates/kronika-registry/src/codec/pg_stat_progress_vacuum.rs`, `bins/pg_kronika-collector/src/main_sources.rs` | `crates/kronika-bdd/features/pg_stat_progress_vacuum.feature`, `docs/type-registry/postgresql.md` |
| `BASE-M03` | Multiline DETAIL/HINT/CONTEXT/STATEMENT и bounded log ingestion | `crates/kronika-source-log/src/collector.rs`, `crates/kronika-registry/src/codec/pg_log.rs` | `crates/kronika-bdd/features/pg_log.feature`, source-log unit tests |
| `BASE-M04` | Statements содержат WAL/temp поля, но кандидаты пока выбираются только по времени/calls | `crates/kronika-source-pg/src/statements.rs`, `crates/kronika-registry/src/codec/pg_stat_statements.rs` | `crates/kronika-bdd/features/pg_stat_statements.feature`, `docs/type-registry/postgresql.md` |
| `BASE-M05` | Operational index flags, включая `indisvalid`/`indisready` | `crates/kronika-source-pg/src/user_indexes.rs`, `crates/kronika-registry/src/codec/pg_stat_user_indexes.rs` | `crates/kronika-bdd/features/user_tables.feature`, `docs/type-registry/postgresql.md` |
| `BASE-M06` | Aggregate prepared transactions | `crates/kronika-source-pg/src/prepared_xacts.rs`, `crates/kronika-registry/src/codec/pg_prepared_xacts.rs`, `bins/pg_kronika-collector/src/main_sources.rs` | `crates/kronika-bdd/features/pg_prepared_xacts.feature`, `docs/type-registry/postgresql.md` |
| `BASE-M07` | Точечные extversion probes для statements/store-plans | `crates/kronika-source-pg/src/statements.rs`, `crates/kronika-source-pg/src/store_plans.rs`, `bins/pg_kronika-collector/src/statements_source.rs`, `bins/pg_kronika-collector/src/plans_source.rs` | unit tests в обоих source-модулях; `crates/kronika-bdd/features/pg_stat_statements.feature`, `crates/kronika-bdd/features/pg_store_plans.feature`, `docs/type-registry/postgresql.md` |
| `BASE-M08` | Freeze-horizon top-N | `crates/kronika-source-pg/src/incident_gauges.rs`, `crates/kronika-registry/src/codec/incident_gauges.rs`, `bins/pg_kronika-collector/src/{pool_sources.rs,buffering.rs}` | unit tests в source/registry-модулях; `bins/pg_kronika-web/src/tests/incidents.rs`, `docs/type-registry/postgresql.md` |

Закрытое решение `DEC-M01` (`Отклонено`): `pg_database_size()` не
добавляется. Production contract находится в
`crates/kronika-source-pg/src/database.rs::database_query`, а тест
`query_includes_version_specific_columns` явно запрещает вызов во всех
layout. Будущий `T2-11` использует approximate contract и не возвращает exact
bytes.
