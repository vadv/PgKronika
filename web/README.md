# PgKronika web UI

The operator interface: a React application that is compiled into the
`pg_kronika-web` binary and served offline from a database host. There is no
runtime network access — everything the page needs ships inside the binary.

## Requirements

- **Node 22**, pinned in `.nvmrc` and `package.json` `engines`. CI reads the
  same `.nvmrc`. Node 24 and newer break the test suite: they define a global
  `localStorage` that shadows the one jsdom installs, and `theme.test.ts` fails
  with `localStorage.clear is not a function`.
- **GNU tar** for packing the asset tarball. macOS ships bsdtar, which rejects
  `--sort=name`; `brew install gnu-tar` provides `gtar`, which the `Makefile`
  picks up automatically.

## How the UI reaches the binary

```
web/src/          vite build      bins/pg_kronika-web/static/   (gitignored)
      │  ───────────────────────▶            │
      │                                       │ tar --sort=name --mtime=@0
      │                                       ▼
      │                         bins/pg_kronika-web/static.tar.gz   (committed)
      │                                       │ build.rs
      │                                       ▼
      └──────────────────────────▶  OUT_DIR/static  →  rust-embed  →  binary
```

The build output is not tracked as a tree — it produced thousand-line
generated diffs on every change. One deterministic tarball is committed
instead: identical sources give an identical archive, so the file is
reviewable as a checksum rather than as content.

**A change under `web/src/` is not complete until `make web-frontend` has been
re-run in the same commit.** Otherwise the binary keeps serving the previous UI
while the diff claims otherwise. CI enforces this by rebuilding the tarball and
failing on any difference.

## Commands

| Command                   | What it does                                                                           |
| ------------------------- | -------------------------------------------------------------------------------------- |
| `make web-frontend-check` | `tsc --noEmit`, `eslint --max-warnings 0`, `prettier --check`, `vitest run --coverage` |
| `make web-frontend`       | `vite build` plus deterministic tarball packing                                        |
| `make web-bundle-budget`  | Fails if `static.tar.gz` exceeds `WEB_BUNDLE_BUDGET_BYTES` (256 KiB)                   |
| `npm run format`          | Prettier: reformat everything except `schema.d.ts` and the lockfile                    |
| `npm run dev`             | Vite dev server, proxying `/v1` to `127.0.0.1:8080`                                    |
| `npm run demo:stub`       | Static API stub on fixture data, for screenshots without a database                    |
| `npm run demo:shot`       | Puppeteer screenshots of the stub in both themes                                       |

Formatting is Prettier's job, not the reviewer's: run `npm run format` before
committing; CI only checks. The bundle budget exists because the tarball ships
inside the web binary on database hosts — raise `WEB_BUNDLE_BUDGET_BYTES`
deliberately, in its own commit. CI additionally runs
`npm audit --omit=dev --audit-level=high`: production dependencies are what
lands in the bundle; dev-dependency updates arrive through Dependabot.

Coverage thresholds live in `vitest.config.ts`. Bootstrap and type-only modules
(`main.tsx`, `i18n/index.ts`, `api/types.ts`) are excluded from the denominator:
keeping them in would only invite fake tests to hold the number up.

## Layout

- `src/api/` — HTTP client and TanStack Query hooks, one module per endpoint.
- `src/state/` — URL-synced analysis state. The URL is the single source of
  truth for `source`, `at`, `span`, `baseline`, `view`, `preset`, `focus`, `q`
  and `sort`, so any screen is a paste-able link. Personal chrome — theme,
  density, language — is deliberately kept out of it.
- `src/components/` — panels.
- `src/design/` — tokens and themes. Components read tokens; a hardcoded color
  is a light-theme bug waiting for its first user.
- `src/i18n/` — `ru` and `en` catalogs, kept at key parity by a test.
- `scripts/` — the demo stub and screenshot harness.

## Data contract

Response shapes come from `bins/pg_kronika-web/openapi/`, which is generated
from the Rust handlers and gated against drift in CI. The UI reads that
contract; it does not invent fields.

`src/api/schema.d.ts` is generated from the OpenAPI tree by
`openapi-typescript` (`npm run codegen`, or `make web-codegen` from the
repo root); the committed file is gated against drift in CI, like the
tree itself. `src/api/types.ts` only re-exports the generated schemas
under stable aliases — add an alias there instead of hand-writing a DTO
or importing `schema.d.ts` from components. Calls go through `apiGet`,
whose path and query parameters are type-checked against the schema.

Errors carry a stable `code` plus `params`; there is no human-readable message
on the wire, and the UI renders the code through the i18n catalog.
