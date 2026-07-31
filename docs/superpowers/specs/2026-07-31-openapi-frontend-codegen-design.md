# OpenAPI → фронтенд: кодеген типов, типизированный клиент, линт спеки

Дата: 2026-07-31. Статус: IMPLEMENTED в PR #150
(`0fbc3dff79a12496d9c1ca16f7208a96da880fa3`).

## Проблема

Контур «код → спека» закрыт: utoipa генерит OpenAPI 3.1, многофайловое
YAML-дерево `bins/pg_kronika-web/openapi/` закоммичено и проверяется в CI
на свежесть (`make openapi` + `git diff`). Контур «спека → фронтенд» —
дыра: `web/src/api/types.ts` (~14 интерфейсов) написан руками и зеркалит
Rust DTO на честном слове; query-параметры собираются вручную через
`URLSearchParams`; в тестах голые `vi.stubGlobal("fetch", ...)` с
нетипизированными фикстурами. Дрейф контракта ловится только в рантайме.

## Решение (вариант B — лёгкий кодеген)

Рассматривались: (A) Orval — генерит react-query хуки и MSW-моки из
спеки; отклонён как оверкилл для 3 потребляемых эндпоинтов из 16 и как
источник нестабильного генерируемого кода (шаблоны меняются между
версиями). (C) только линтер без кодегена — не закрывает дрейф типов.

Выбран **B: openapi-typescript + openapi-fetch + Spectral**:

1. **Кодеген типов.** `openapi-typescript` генерит
   `web/src/api/schema.d.ts` из закоммиченного дерева
   `bins/pg_kronika-web/openapi/openapi.yaml` (внешние `$ref`
   разрешаются из коробки). Файл коммитится — тот же паттерн, что у
   openapi-дерева и `static.tar.gz`. Команда: `npm run codegen`
   (web/), таргет `make web-codegen`.
2. **Типизированный клиент.** `openapi-fetch` (~5 КБ, zero deps) как
   транспорт: `client.GET(path, { params })` проверяет путь и
   query-параметры на уровне типов. Поверх — обёртка `apiGet`,
   сохраняющая текущую семантику ошибок: `application/problem+json` →
   `ApiError` (бросается, как сейчас; react-query продолжает работать
   через throw). Ручная сборка `URLSearchParams` уходит.
3. **`types.ts` становится тонким ре-экспортом** из `schema.d.ts`
   (алиасы на `components["schemas"][...]`), имена приводятся к
   каноническим из спеки (`SummaryResponse` → `ViewSummaryResponse` и
   т.п.). Расхождения ручных типов со спекой (например, единый
   `QualityMeta` против раздельных `SummaryQuality`/`HeatmapQuality`)
   устраняются в пользу спеки.
4. **Типизированные фикстуры в тестах.** Фикстуры помечаются
   `satisfies components["schemas"][...]` — невалидная фикстура ломает
   typecheck. MSW не вводится (YAGNI на текущем объёме).
5. **Линтер спеки.** Spectral (`@stoplight/spectral-cli`, devDep web/),
   ruleset `.spectral.yaml` в корне (extends `spectral:oas`,
   стилистические шумные правила приглушены). Линтуется **бандл**
   (`make openapi-bundle` → `target/pg-kronika-openapi.yaml`), а не
   дерево: многофайловые `$ref` вида `paths/ui.yaml#/~1v1~1...` Spectral
   разрешает криво (ложные `oas3-schema` errors и `unused-component`
   warnings). Таргет `make openapi-lint` = бандл + spectral.

## CI

- В job `lint` (cargo уже есть, экспортёр для `make openapi` там же
  собирается) добавляется шаг `make openapi-lint`.
- В job `frontend` (Node запинен через `web/.nvmrc`) добавляется
  freshness-check типов: `make web-codegen` + `git diff --exit-code --
  web/src/api/schema.d.ts` (дрейф спека→типы = красный CI).

Существующий drift-check «код→спека» в job `lint` не меняется. Цепочка
становится полной: Rust DTO → спека (drift-check + spectral) → TS-типы
(freshness-check) → typecheck фронтенда.

## Границы

- Хуки react-query остаются руками (их три; Orval-генерация хуков —
   возможный следующий шаг при росте числа потребляемых эндпоинтов).
- MSW-моки из спеки не вводятся.
- Schemathesis остаётся ручным workflow, перенос в PR-CI — отдельная
   задача.
- Rust-код и сама спека не меняются (только потребление).

## DoD

- `make web-frontend-check` зелёный (typecheck/lint/vitest, включая
  обновлённые фикстуры);
- `make openapi-lint` зелёный;
- повторный `npm run codegen` даёт пустой дифф;
- новые CI-шаги описаны в `.github/workflows/ci.yml`;
- изменение DTO на бэке без регенерации типов = красный CI (проверено
  локально сломанной регенерацией).
