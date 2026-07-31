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

| Command | What it does |
|---|---|
| `make web-frontend-check` | `tsc --noEmit`, `eslint --max-warnings 0`, `vitest run --coverage` |
| `make web-frontend` | `vite build` plus deterministic tarball packing |
| `npm run dev` | Vite dev server, proxying `/v1` to `127.0.0.1:8080` |
| `npm run demo:stub` | Static API stub on fixture data, for screenshots without a database |
| `npm run demo:shot` | Puppeteer screenshots of the stub in both themes |

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

`src/api/types.ts` currently restates those schemas by hand, which has already
produced one divergence. Treat every hand-maintained DTO as a defect awaiting
generation from the OpenAPI tree, not as a pattern to copy.

Errors carry a stable `code` plus `params`; there is no human-readable message
on the wire, and the UI renders the code through the i18n catalog.
