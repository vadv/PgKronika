# Plan Forensic Detail — дизайн

## Цель

Выбранный план должен становиться отдельным forensic-экраном в утверждённом
визуальном языке PgKronika. Экран отвечает на три вопроса: как выглядел
сохранённый план, как менялись наблюдавшиеся метрики этого `planid` в выбранном
окне и куда продолжить расследование по тому же запросу.

## Источники и семантика

- Point entity отдаёт bounded `plan`, `planid`, `queryid`, текущие delta-метрики
  и first/last call.
- History entity ограничен шестью часами, `limit=96` и пятью числовыми полями:
  `calls`, `mean`, `rows`, `shared_hit`, `shared_read`.
- Point entity с `include=related` возвращает все наблюдения Statements,
  совпавшие с plan attribution текущего collector fork. Они показаны как
  продолжение расследования, без ранжирования «надёжности» и без causal copy.
- Формат `plan` не предполагается заранее: pg_store_plans может отдавать text,
  JSON, XML, YAML или raw. Интерфейс сохраняет исходный текст и форматирует JSON
  только когда обычный `JSON.parse` успешен.
- Один plan snapshot не позволяет честно рисовать changed nodes или A/B diff.
  Detail не синтезирует diff; кнопка «остальные планы запроса» возвращает в
  отфильтрованный Plans workspace.

## Композиция desktop

Экран занимает всё пространство между 60 px Health Line и 24 px status bar.

1. **Entity strip, 40 px.** Plans → plan id, query id, snapshot, first/last call,
   collection state, закрытие.
2. **Temporal field.** Четыре синхронных слоя:
   - observed snapshots — 96-bucket максимум, без линии между отсутствующими
     наблюдениями;
   - mean latency + calls;
   - rows;
   - shared hit + shared read.
3. **Continuation lane.** Statements candidates и возврат к остальным planid
   того же queryid.
4. **Три нижних колонки.** Сохранённый plan text/tree; current/first-observed
   matrix и call window; все Statements candidates и подготовленные переходы.

На 1920×1080 root не скроллится. Plan text и каждая нижняя колонка имеют
независимую прокрутку при длинном payload.

## Поведение

- Desktop `dock=row&view=plans` монтирует PlanDetail вместо generic dock.
- Plans overview остаётся смонтированным и скрытым, чтобы закрытие возвращало
  пользователя в тот же virtual-scroll context.
- `Escape` и явная кнопка закрытия удаляют только `dock=row`.
- Открытие Statement candidate сохраняет timestamp этого наблюдения.
- «Запрос в Statements» устанавливает `view=statements&preset=time` и server
  filter `queryid=…`.
- «Остальные планы запроса» устанавливает `view=plans&preset=change_timeline`
  и тот же server filter.
- Mobile сохраняет существующий generic dock.

## Визуальные ограничения

- Основа — утверждённый Superdesign detail: тонкие ruled-разделители, 4 px
  ритм, моноширинные числа, синий primary signal и янтарный secondary signal.
- Plan body — главный evidence-артефакт, а не декоративная карточка.
- Raw entity token, endpoint, provenance method, gap/gated counters и язык
  доказательства не попадают в primary surface.
- Missing значения локальны и подписаны «не наблюдалось»; они не превращают
  весь экран в warning banner.

