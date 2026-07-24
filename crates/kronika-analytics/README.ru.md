# kronika-analytics

[English version](README.md)

`kronika-analytics` содержит независимые от источника разности счётчиков,
поиск аномалий и ядро контрактов, которое использует production timeline
overview.

## Ядро контрактов overview

Модуль `overview` определяет retained event observations, точные event counts,
coverage, редукции счётчиков и gauges, health-оценку и интерфейс адаптера для
семантического сравнения. Он не читает PGM, не хранит overview index, не
обслуживает HTTP и не задаёт response redaction.

Контракт identity различает два случая:

- sealed или заранее доказанный locator даёт стабильный при повторном rebuild
  content-derived lineage из source scope, naming contract, segment locator и
  первого catalog descriptor;
- live view без доказанного будущего locator использует отдельный discriminator
  и возвращает `IdentityQuality::Approximate`.

Catalog ordinal считается по всему сегменту. Counter intervals и state changes
являются derived facts, а не retained event observations. Event payload
сохраняет поля типизированной строки PostgreSQL log; machine kind не является
диагнозом. В частности, raw signal сам по себе не доказывает OOM.

Event counts хранят совместную severity/category/SQLSTATE истину и выводят из
неё marginal counts с проверяемой арифметикой. Web-проекция раздельно сообщает
число retained error occurrences, retained error groups и retained observation
rows. Суммы по severity и category, SQLSTATE buckets top/other/missing и joint
buckets top/other обязаны независимо сходиться с числом retained error
occurrences. Выбор важных событий также ограничен и детерминирован, а число
пропущенных элементов остаётся явным.

## Редукции и health

Counter pair требует один series, возрастающее время, общий reset epoch и
отсутствие известного gap. Пара принадлежит bucket текущего sample; редукция
отмечает evidence, пересекающий границу bucket. Ratio разрешён только при
совпадающих границах пар numerator и denominator.

Gauge input отклоняет нечисловые и бесконечные значения. Ограниченная gauge
reduction хранит канонический набор samples, поэтому merge частей даёт тот же
mean, что и редукция полного набора. Zero-order hold включается явно и
останавливается на известном gap.

Health score требует явный penalty и строгое полное покрытие интервала для
каждого применимого обязательного factor. Partial coverage, loss, предположение
о старом period, cadence boundary или неизвестная exactness оставляют numeric
score отсутствующим. Проверенный policy floor может дать `Critical`, не
подставляя числовой ноль. Downsample сначала выбирает floor cell, иначе — cell
с минимальным numeric score и детерминированными tie-breaks.
Floor может установить только structured или derived-exact evidence:
`PANIC` такого качества доказывает availability, а `XX001` или `XX002` такого
же качества — integrity. Parsed или heuristic evidence, child termination и
`53100` остаются важными, но сами по себе floor не устанавливают.

## Границы и ошибки

Sparse count keys, counter pairs, gauge samples, возвращаемые observations и
oracle coverage spans ограничиваются параметрами вызывающего кода. Overflow и
превышение лимитов возвращаются типизированно. Вариант materialized query до
клонирования учитывает каждую observation, её boxed payload, сохранённый текст
и loss storage в переданном вызывающим кодом byte limit. Один oracle query
возвращает observations, counts и coverage из одного pinned вызова адаптера.
`MemoryOracle` служит только для fixtures над уже декодированными records;
production reader-backed adapter находится в `pg_kronika-web`, вне этого крейта.

Public surface перечислен в [`src/lib.rs`](src/lib.rs).
