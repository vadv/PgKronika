# Подсветка аномальных значений: исследование, эталон и каталог порогов

Дата: 2026-07-29. Исследование перед спекой. Базируется на main после
#137. Источники: два прогона многоагентного deep-research (verified,
42 первичных источника) и разбор эталонного проекта reftool, где
подсветка уже реализована.

## Задача

Интерфейс должен подсвечивать значения, «выбивающиеся из нормы»:
запрос длиннее обычного, приближение к лимиту, saturation ресурса.
В PgKronika такой подсветки как продуктовой сущности нет. Нужно:
какие метрики подсвечивать по абсолютному порогу, какие — только
относительно собственной истории, каким методом, на каком слое.

## Два класса детекции — отраслевой консенсус

- **Класс 1 — абсолютные пороги.** Метрика привязана к физическому или
  протокольному лимиту; «плохое» значение одинаково для любого инстанса,
  baseline не нужен, значения сравнимы между хостами.
- **Класс 2 — относительно baseline.** Универсального «плохо» нет;
  подсвечивать нужно отклонение от нормы конкретной сущности, выученной
  из её истории.

Диагностический признак: **нет подтверждённого универсального порога →
метрика Класс 2** (lock-wait абсолютный, temp-файлы, disk MB/s,
blks_read rate — консенсусной отсечки нет).

## Эталон: как это сделано в reftool

reftool держит подсветку на **двух слоях**, и это ключевой факт для
нашей архитектуры:

1. **Per-cell раскраска — в клиенте** (`crates/reftool-web/frontend/
   src/utils/thresholds.ts`, 1014 строк, ~120 классификаторов).
   Каждая колонка таблицы имеет функцию `value → level`, level ∈
   {critical, warning, good, inactive}. Пороги абсолютные, часть
   context-aware по соседним полям строки. Считается в браузере,
   потому что значение уже загружено в строку — сравнение бесплатно.
2. **Семантический анализ — в ядре** (`reftool-core/src/analysis/`):
   `rules/` (21 доменный файл) порождают события, `advisor/` (13
   файлов) — именованные находки: «Checkpoint storm — forced
   checkpoints», «Lock cascade — session storm», «Table bloat — dead
   tuples accumulating», «Query plan regression», «Write amplification
   storm», «Autovacuum saturating disk I/O», «Buffer eviction pressure
   — increase shared_buffers», «Connection storm», «VACUUM blocked by
   idle transactions», «Container CPU throttling», «Synchronous
   replication waits». Это аналог линз PgKronika.

Разделение труда: **абсолютные per-cell пороги (Класс 1) — на клиенте,
семантические инциденты и baseline — в ядре.** Расплата reftool за это —
пороги живут в двух местах (TS thresholds + Rust rules), рассинхрон не
ловится.

Слабость reftool-подсветки: она «немая». Ячейка красная, но почему —
из tooltip не следует ни порог, ни baseline. Макет PgKronika v5
требует объяснимую подсветку: «crit: mean > 100ms · baseline 9.5ms
(×11.7)». Порог + baseline знает ядро, не клиент.

## Полный каталог из reftool, сверенный с индустрией

Пороги ниже — из `thresholds.ts`. Колонка «индустрия» — совпадение или
расхождение с verified-дефолтами (awesome-prometheus, pganalyze, kernel
PSI). Класс: A — абсолютный, B — baseline/относительный, A* —
абсолютный, но привязан к контексту строки (cores, total, state).

### CPU, load, планирование

| Метрика | warn | crit | Класс | Индустрия |
| --- | --- | --- | --- | --- |
| cpu_pct (процесс) | ≥50 | ≥90 | A | 100% ядра = bottleneck (USE) |
| load.avg1 / cores | >1× | >2× | A* | совпадает: load1/nproc>2 |
| host idle_pct | <30 | <10 | A | — |
| iow_pct (iowait) | >5 | >15 | A | — |
| steal_pct | >3 | >10 | A | — |
| load.procs_blocked (D) | >0 | >4 | A | USE saturation |
| backend_load / cores | ≥0.25 | ≥0.5 | A* | — |

### Память, swap

| Метрика | warn | crit | Класс |
| --- | --- | --- | --- |
| mem_pct | ≥70 | ≥90 | A |
| vgrow_kb / rgrow_kb (рост) | >100MB | >1GB | A |
| vswap_kb (в свопе) | >0 | >100MB | A |
| swap.used_kb | >0 | >1GB | A |
| swin_s / swout_s (vmstat) | — | >0 | A |
| majflt (major page faults) | >100 | >10000 | A |
| rss_kb | >1GB | >4GB | A |

### PSI

| Метрика | warn | crit | Класс | Индустрия |
| --- | --- | --- | --- | --- |
| cpu_some_pct | ≥5 | ≥25 | A | окно avg10/60/300; отсечку reftool задаёт, kernel — нет |
| mem_some_pct | ≥5 | ≥25 | A | |
| io_some_pct | ≥10 | ≥40 | A | |

reftool закрывает пробел, оставшийся в ресёрче: **числовые отсечки PSI
(5/25 для cpu/mem, 10/40 для io на `some`)** — эмпирические, стартовые.
Для CPU используется `some` (system-wide `full` для CPU не определён).

### cgroup (контейнер)

| Метрика | warn | crit | Класс |
| --- | --- | --- | --- |
| cgroup_cpu.used_pct | ≥70 | ≥90 | A |
| cgroup_cpu.throttled_ms | >0 | >100 | A |
| cgroup_cpu.nr_throttled | >0 | — | A |
| cgroup_memory.anon_pct | ≥70 | ≥90 | A |
| cgroup_memory.headroom_pct | <20 | <10 | A |
| cgroup_memory.oom_kills | — | >0 | A |

### Диск, IO

| Метрика | warn | crit | Класс |
| --- | --- | --- | --- |
| disk.util_pct | ≥60 | ≥90 | A |
| disk.max/r/w_await_ms | ≥2 | ≥10 | A |
| disk.space_used (free) | <20% и <15GB | <10% и <15GB | A* |
| blkdelay_s | >10 | >50 | A |
| disk_blks_read_s | >0 | — | A |
| network.errors_s / drops_s | >0 | >10 | A |

### PostgreSQL: сессии, активность

| Метрика | warn | crit | Класс |
| --- | --- | --- | --- |
| query_duration_s (не idle) | ≥1 | ≥30 | A* |
| xact_duration_s | ≥5 | ≥60 | A |
| state = idle in transaction | warning | (aborted)=crit | A |
| sessions.active | >50 | >100 | A |
| sessions.idle_in_tx | — | >0 | A |
| pg.blocked_sessions | >0 | ≥5 | A |
| pg.long_queries | >0 | ≥5 | A |
| pg.long_tx | >0 | ≥3 | A |
| pg.rollback_ratio | >3 | >10 | A |
| wait_event_type ≠ null | warning | — | A |
| lock_granted = false | — | critical | A |
| deadlocks | — | >0 | A |

Расхождение с pganalyze: reftool жёстче — idle-in-tx любой длительности
подсвечивает сразу, pganalyze warn только с 1800s. Для снапшотной
модели (reftool/Kronika видят факт в моменте) жёсткость оправдана.

### PostgreSQL: кэш, bgwriter, checkpoint

| Метрика | warn | crit | Класс | Индустрия |
| --- | --- | --- | --- | --- |
| hit_pct / io_hit_pct / effective_hit_pct | <99 | <90 | A | совпадает (<98 warn) |
| pg.checkpoints_per_min | >2 | — | A | checkpoint storm |
| bgwriter.checkpoint_write_time_ms | >30s | >120s | A | |
| buffers_backend_s | >0 | — | A | eviction pressure |
| maxwritten_clean | >0 | — | A | |
| client_evictions_s | <10 | ≥10 | A | |

### PostgreSQL: таблицы, dead tuples, vacuum, TOAST

| Метрика | warn | crit | Класс | Индустрия |
| --- | --- | --- | --- | --- |
| dead_pct | ≥5 | ≥20 | A | ≥10% + >10k floor |
| n_dead_tup | ≥1000 | ≥100000 | A | |
| seq_pct | ≥30 | ≥80 | A | |
| n_mod_since_analyze | ≥100k | ≥1M | A | |
| n_ins_since_vacuum | ≥100k | ≥1M | A | |
| last_autovacuum age (если dead>0) | >6h | >24h | A* | |
| last_autoanalyze age (если mod≥10k) | >6h | >24h | A* | |
| temp_bytes_s | >0 | — | A | (только indicator) |

### PostgreSQL: statements, plans (регрессии)

| Метрика | warn | crit | Класс |
| --- | --- | --- | --- |
| time_ratio / query_time_ratio | ≥2 | ≥10 | B (регрессия к своему) |
| cv (coefficient of variation) | ≥1 | ≥3 | B |
| ms_per_row | ≥10 | ≥100 | A |
| mean_time_ms | ≥10 | ≥100 | A |
| pct_time | ≥20 | ≥50 | A |
| plan_time_pct | ≥50 | ≥80 | A |
| plan_count | >1 | >3 | A |

`time_ratio`/`cv` — уже Класс 2 по сути: отношение к собственному
среднему/дисперсии. Это ровно то, что ядро Kronika считает через
modified z-score.

### Errors / events (лог)

| Метрика | правило | Класс |
| --- | --- | --- |
| severity | PANIC/FATAL=crit, ERROR=warn | A |
| level / category | data_corruption/resource/system=crit | A |
| count | ≤10 нет, ≤100 warn, >100 crit | A |
| event_type | server_crash=crit, checkpoint_too_frequent/lock_wait=warn | A |
| elapsed_s | context по event_type (lock_wait/slow_query/vacuum) | A* |

### Replication

| Метрика | warn | crit | Класс | Индустрия |
| --- | --- | --- | --- | --- |
| pg.replay_lag_s | >10 | >60 | A/пограничный | pganalyze 100MB/1024MB; «подстрой под baseline» |
| pg.conflicts | >0 | — | A |

## PgKronika: что уже есть, чего нет

- **Класс 2 реализован**: `crates/kronika-analytics/src/anomaly/` —
  modified z-score (MAD), robust к выбросам. Grafana Robust (median+MAD)
  подтверждает выбор. `time_ratio`/`cv` из каталога reftool ложатся сюда.
- **Класс 1 отсутствует** — весь каталог выше нужно ввести.
- **Семантический слой есть** — линзы/инциденты (аналог reftool advisor).
- **Per-cell подсветки нет** — макет v5 её рисует, бэкенд не отдаёт.

## Слой: каталог в ядре (решено)

Весь каталог порогов переносится **в аналитическое ядро** Kronika как
чистые функции `snapshot → verdict{level, threshold, observed}`. Клиент
пороги не считает — красит ячейку по готовому вердикту. Расцветка в
клиенте не рассматривается.

reftool держит абсолютные пороги в клиентском `thresholds.ts` — мы берём
из него метрики и логику, но кладём их в ядро, по трём причинам:

- **Единый источник.** У reftool пороги в двух местах (TS + Rust-
  семантика), рассинхрон не ловится. У нас один каталог в ядре.
- **Объяснимость.** Вердикт несёт порог и фактическое значение — макет
  v5 требует tooltip «crit: mean > 100ms · baseline 9.5ms (×11.7)».
  reftool-подсветка «немая»: цвет без причины.
- **Класс 2 уже в ядре.** baseline/z-score клиент вычислить не может
  (reference-распределение в ядре) — вердикт рождается в ядре по
  определению; Класс 1 живёт рядом, не отдельной веткой на клиенте.

Каталог — dependency-free чистые функции: детерминированы, без I/O,
тестируются как z-score. Config-bound пороги (`max_connections`,
`autovacuum_*`) берут параметры из уже собранного снапшота `pg_settings`.

## Развенчано проверкой

- «Порог 70% utilization из USE-метода» — миф (verified-опровержение).
- «Saturation мерить прогнозом времени до заполнения» — опровергнуто.

## Открытые калибровочные вопросы (на демо-стенд)

- Отсечки PSI: reftool даёт эмпирические 5/25 и 10/40 — проверить на
  реальных данных стенда против verified-окон avg10/60/300.
- Абсолютный порог lock-wait и temp — консенсуса в индустрии нет;
  reftool трактует «любой lock-wait = warning», temp — только indicator.
  Кандидаты в Класс 2, если стенд не покажет универсальную отсечку.

## Следующий шаг

Спека каталога Класса 1: чистые функции ядра `snapshot → verdict` по
доменам выше, с разделением universal-констант и config-bound
(`max_connections`, `autovacuum_*`), уровнями severity и контрактом
per-cell вердикта (level + threshold + observed) в web→frame.
Полнота — не ниже каталога reftool (~120 метрик); числа — стартовые,
tunable, финализируются калибровкой на стенде.
