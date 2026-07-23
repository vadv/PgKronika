# kronika-analytics

[English version](README.md)

`kronika-analytics` содержит независимые от источника разности счётчиков,
поиск аномалий и ядро контрактов будущего timeline overview.

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

## Границы и ошибки

Sparse count keys, counter pairs, gauge samples, возвращаемые observations и
oracle coverage spans ограничиваются параметрами вызывающего кода. Overflow и
превышение лимитов возвращаются типизированно. Один oracle query возвращает
observations, counts и coverage из одного pinned вызова адаптера.
`MemoryOracle` служит только для fixtures над уже декодированными records;
production raw и index adapters здесь не реализованы.

Public surface перечислен в [`src/lib.rs`](src/lib.rs).
