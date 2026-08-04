# PR208 Plan Forensic Detail — план реализации

**Goal:** заменить generic Plans dock полноэкранным forensic detail, добавить
обратное Plans → Statements продолжение и сохранить утверждённую композицию на
1920×1080.

## Task 1. Backend relation contract

- Сделать Plans `capabilities.related=true`.
- Добавить fork-aware `plan_statement` resolver на том же snapshot.
- Покрыть OSSC, vadv и несовпадающий queryid Rust-тестами.

## Task 2. Frontend RED contract

- Добавить `PlanDetail.test.tsx` с четырьмя temporal lanes, bounded plan body,
  тремя analysis columns и всеми returned Statement candidates.
- Зафиксировать один point related request и 96×5 history до шести часов.
- Добавить App deep-link test: full canvas, preserved hidden overview, no generic
  dock, Escape возвращает overview.

## Task 3. Реализация PlanDetail

- Реализовать entity strip, observed-snapshot lane, три numeric lanes,
  continuation lane и три нижних evidence columns.
- Сделать plan payload format-agnostic и independently scrollable.
- Добавить переходы в Statement candidate, Statements filter и Plans-by-query.
- Добавить EN/RU copy без технической linkage-обвязки.

## Task 4. Demo и real-browser verifier

- Сделать demo plan многострочным, history — различимым, queryid — общим для
  нескольких plan versions, а related Statements — inspectable.
- Проверить 1920×1080, root scroll 0, detail y=136..1056, четыре lanes, три
  analysis columns, bounded request и все три investigation continuation.
- Снять `forensic-plan-detail-1920x1080.png`.

## Task 5. Visual QA и поставка

- Сравнить reference и prototype одним input на одинаковом viewport.
- Один раз прогнать Impeccable detector на изменённых UI targets.
- Прогнать frontend coverage, Rust relation tests, shell verifier, production
  audit, deterministic static tar и bundle budget.
- Открыть PR208, дождаться зелёного CI и слить в `main`.

