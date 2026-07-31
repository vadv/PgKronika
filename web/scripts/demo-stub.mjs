#!/usr/bin/env node
// Demo stub for the v6 summary+heatmap shell: serves the built SPA from
// bins/pg_kronika-web/static plus the three endpoints the shell consumes,
// backed by a rich deterministic fixture (PRNG seed 42).
//
// The catalog fixture is the verbatim `GET /v1/ui/catalog` response of a live
// pg_kronika-web binary against an empty store (everything `gated`); the stub
// flips every availability to "available" at load so all tabs render.

import { createServer } from "node:http";
import { readFile } from "node:fs/promises";
import { extname, join, normalize } from "node:path";
import { fileURLToPath } from "node:url";

const HERE = fileURLToPath(new URL(".", import.meta.url));
const STATIC_DIR = normalize(join(HERE, "../../bins/pg_kronika-web/static"));
const PORT = Number(process.env.PGK_DEMO_PORT ?? 18444);

const MIME = {
  ".html": "text/html; charset=utf-8",
  ".js": "text/javascript; charset=utf-8",
  ".css": "text/css; charset=utf-8",
  ".svg": "image/svg+xml",
  ".png": "image/png",
  ".woff2": "font/woff2",
};

// --- catalog ---------------------------------------------------------------

const catalog = JSON.parse(
  await readFile(join(HERE, "catalog.fixture.json"), "utf8"),
);

for (const view of catalog.views) {
  view.availability = "available";
  for (const group of [view.inputs, view.metrics, view.columns]) {
    for (const item of group) item.availability = "available";
  }
}

// --- summary ---------------------------------------------------------------

// Populations per plan: activity..events in stable view_code order.
const POPULATIONS = [142, 500, 83, 64, 121, 2, 218, 3, 5];

function summaryResponse(at) {
  return {
    at_us: at,
    views: catalog.views.map((view, i) => ({
      view: view.code,
      snapshot_ts_us: at,
      population: POPULATIONS[i] ?? 0,
      status: "complete",
      notable: false,
    })),
    quality: {
      status: "complete",
      snapshots: 48,
      gaps: [],
      gated: [],
      unavailable_revision: [],
      resource_limited: [],
      active_tail: true,
    },
  };
}

// --- heatmap ---------------------------------------------------------------

// Deterministic PRNG (mulberry32), seeded once per request so every run of
// the stub renders the identical picture.
function mulberry32(seed) {
  let a = seed >>> 0;
  return () => {
    a = (a + 0x6d2b79f5) >>> 0;
    let t = a;
    t = Math.imul(t ^ (t >>> 15), t | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

const ENTITIES = {
  statements: [
    ["stmt:7101", "UPDATE orders SET status=$1 WHERE id=$2"],
    ["stmt:7102", "SELECT * FROM sessions WHERE user_id=$1"],
    ["stmt:7103", "INSERT INTO events (ts,kind,payload) VALUES ($1,$2,$3)"],
    ["stmt:7104", "SELECT count(*) FROM ledger WHERE account_id=$1"],
    ["stmt:7105", "DELETE FROM cart_items WHERE expires_at < $1"],
    ["stmt:7106", "SELECT o.id, o.total FROM orders o JOIN users u ON u.id=o.user_id"],
    ["stmt:7107", "UPDATE inventory SET qty=qty-$1 WHERE sku=$2"],
    ["stmt:7108", "VACUUM ANALYZE public.audit_log"],
  ],
  activity: [
    ["pid:12041", "app/api-worker (active)"],
    ["pid:12042", "app/api-worker (idle in transaction)"],
    ["pid:12077", "app/reports (active)"],
    ["pid:12105", "etl/loader (active)"],
    ["pid:12130", "pgbouncer (idle)"],
    ["pid:12131", "app/api-worker (active)"],
    ["pid:12188", "autovacuum worker"],
    ["pid:12201", "walwriter"],
  ],
};

const METRIC_STYLE = {
  time: { unit: "ms", scale: 1200 },
  calls: { unit: "count", scale: 8000 },
  io: { unit: "blocks", scale: 40000 },
  temp: { unit: "blocks", scale: 5000 },
  wait: { unit: "us", scale: 900 },
  cpu: { unit: "ratio", scale: 0.9 },
};

const GAP_START = 20; // buckets 20..22 are null in every row
const GAP_END = 22;
const PEAK_BUCKET = 40; // hot peak

function genericEntities(view) {
  return Array.from({ length: 8 }, (_, i) => [
    `${view}:e${i + 1}`,
    `${view} entity ${i + 1}`,
  ]);
}

function heatmapResponse(params) {
  const view = params.get("view") ?? "statements";
  const metric = params.get("metric") ?? "time";
  const from = BigInt(params.get("from") ?? "0");
  const to = BigInt(params.get("to") ?? "86400000000");
  const buckets = Number(params.get("buckets") ?? "56");
  const top = Number(params.get("top") ?? "8");

  const rand = mulberry32(42);
  const style = METRIC_STYLE[metric] ?? { unit: "count", scale: 1000 };
  const entities = (ENTITIES[view] ?? genericEntities(view)).slice(0, top);

  const rows = entities.map(([entity, label]) => {
    const phase = rand() * Math.PI * 2;
    const amp = 0.5 + rand() * 0.5;
    const values = [];
    for (let b = 0; b < buckets; b++) {
      if (b >= GAP_START && b <= GAP_END) {
        values.push(null);
        continue;
      }
      const wave = 0.5 + 0.3 * Math.sin((b / buckets) * Math.PI * 4 + phase);
      const peak = 1.6 * Math.exp(-((b - PEAK_BUCKET) ** 2) / (2 * 3 ** 2));
      const noise = rand() * 0.15;
      values.push(
        Math.round(style.scale * amp * (wave + peak + noise) * 100) / 100,
      );
    }
    const max = Math.max(...values.filter((v) => v !== null));
    return {
      entity,
      label,
      unit: style.unit,
      score: { lower: Math.round(max * 0.8 * 100) / 100, upper: max },
      values,
    };
  });

  const span = to - from;
  const bucketUs = span / BigInt(buckets);
  return {
    grid: {
      from_us: from.toString(),
      to_us: to.toString(),
      bucket_count: buckets,
    },
    ranking: { exact: true, unseen_upper: 0 },
    rows,
    quality: {
      status: "partial",
      snapshots: 47,
      gaps: [
        {
          from_us: (from + bucketUs * BigInt(GAP_START)).toString(),
          to_us: (from + bucketUs * BigInt(GAP_END + 1)).toString(),
        },
      ],
      gated: [],
      unavailable_revision: [],
      resource_limited: [],
      unbounded_segments: [],
      active_tail: false,
    },
  };
}

// --- server ----------------------------------------------------------------

function sendJson(res, body) {
  const payload = JSON.stringify(body);
  res.writeHead(200, {
    "content-type": "application/json",
    "content-length": Buffer.byteLength(payload),
  });
  res.end(payload);
}

async function sendStatic(res, path) {
  try {
    const body = await readFile(join(STATIC_DIR, path));
    res.writeHead(200, {
      "content-type": MIME[extname(path)] ?? "application/octet-stream",
    });
    res.end(body);
  } catch {
    res.writeHead(404, { "content-type": "text/plain" });
    res.end("not found");
  }
}

const server = createServer((req, res) => {
  const url = new URL(req.url ?? "/", "http://stub");
  if (url.pathname === "/v1/ui/catalog") return sendJson(res, catalog);
  if (url.pathname === "/v1/views/summary") {
    return sendJson(res, summaryResponse(url.searchParams.get("at") ?? "0"));
  }
  if (url.pathname === "/v1/timeline/heatmap") {
    return sendJson(res, heatmapResponse(url.searchParams));
  }
  const path = url.pathname === "/" ? "index.html" : url.pathname.slice(1);
  if (path.split("/").includes("..")) {
    res.writeHead(400, { "content-type": "text/plain" });
    return res.end("bad path");
  }
  return sendStatic(res, normalize(path));
});

server.listen(PORT, "127.0.0.1", () => {
  console.log(`demo stub: http://127.0.0.1:${PORT} (static: ${STATIC_DIR})`);
});
