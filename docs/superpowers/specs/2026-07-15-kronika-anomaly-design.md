# Контракт `kronika-anomaly`

`kronika-anomaly` отвечает на ретроспективный запрос: какие серии и колонки
сильно отличались от остального выбранного периода. Крейт содержит только
чистую математику. Registry объявляет классы колонок и scale floor, reader
строит типизированные series, web применяет HTTP-лимиты и сериализацию.

## Скор окна

Для текущего окна `cur` и остальных точек периода `reference`:

```text
median_ref = median(reference)
mad        = median(abs(reference - median_ref))
floor      = max(eps_abs, eps_rel * abs(median_ref))
sigma      = max(1.4826 * mad, floor)
score      = (median(cur) - median_ref) / sigma
```

Позиция оценивается при наличии не менее 20 reference-точек и 3 точек окна.
NaN и бесконечности не превращаются в число. Направление определяется знаком
score. Соседние позиции выше `threshold` объединяются в episode; peak — позиция
с максимальным абсолютным score.

Reference некаузальный: он включает точки до и после окна. Это соответствует
анализу завершившегося периода, но не подходит для потокового алертинга. Эпизод,
занимающий большую часть периода, может изменить собственный baseline.

## Подготовка series

- `Cumulative` скорится по rate из `kronika-diff`.
- `Gauge` скорится по исходным числовым значениям.
- `Label` и `Timestamp` не скорятся.
- `NoData`, `Null`, нечисловые и не-finite значения исключаются и учитываются в
  `nodata_points`.

Каждая section декодируется один раз за запрос, после чего diff/gauge series
строятся один раз на весь период. Перекрывающиеся окна используют готовые
массивы и не вызывают HTTP-адаптер повторно.

## HTTP-контракт

```text
GET /v1/anomalies?source=…&from=…&to=…
  [&window=1h][&step=15m][&threshold=3.5]
  [&eps_rel=0.05][&limit=50][&section=name]
```

Ответ содержит глобально ранжированные episodes, per-section счётчики
`series_total`, `evaluated`, `not_evaluated` и `nodata_points`, а также массив
`skipped`. Отсутствие результата не выдаётся за нулевое измерение.

Один запрос ограничен числом window positions, материализованных reader-ячеек
и суммарными point-position pairs. Section, превысившая лимит, попадает в
`skipped` с причиной; накопленный список episodes ограничивается `limit` во
время сканирования, а не только перед ответом.

## Отложено

Сезонные baseline, drift detectors, потоковый hysteresis и push-alerting не
входят в этот контракт. Их добавление потребует отдельной каузальной модели и
операционного состояния.
