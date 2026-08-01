#!/usr/bin/env node
// Demo stub for the v5/v6 web shell: serves the built SPA from
// bins/pg_kronika-web/static plus every endpoint the shell consumes,
// backed by a coherent "busy orders-db primary" dataset.
//
// All timestamps are computed relative to NOW from the request's own
// from/to/at parameters (int64 µs), so the UI never renders empty no
// matter when the demo runs. Shapes follow web/src/api/schema.d.ts:
// wire int64 µs that the schema declares as decimal strings stay
// strings, schema int64 JSON numbers stay numbers.
//
// The catalog fixture mirrors the `GET /v1/ui/catalog` response of a live
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

const US = 1_000_000;
const nowUs = () => Date.now() * 1000;

// Deterministic PRNG (mulberry32), seeded per request/entity so every run of
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

function hashCode(text) {
  let h = 2166136261;
  for (let i = 0; i < text.length; i++) {
    h = Math.imul(h ^ text.charCodeAt(i), 16777619) >>> 0;
  }
  return h;
}

// --- catalog ---------------------------------------------------------------

const catalog = JSON.parse(
  await readFile(join(HERE, "catalog.fixture.json"), "utf8"),
);

for (const view of catalog.views) {
  view.availability = "available";
  for (const group of [view.inputs, view.metrics, view.columns]) {
    for (const item of group) {
      // `not_collected` is intrinsic to the source (the collector never
      // writes the value), not a property of the empty demo store.
      if (item.availability !== "not_collected")
        item.availability = "available";
    }
  }
}

// --- summary ---------------------------------------------------------------

// Populations per plan: activity..events in stable view_code order.
const POPULATIONS = [142, 500, 83, 64, 121, 2, 218, 3, 5];
// Views that carry a live anomaly in the demo storyline.
const NOTABLE = { locks: ["critical", 4], statements: ["warning", 2] };

function summaryResponse(at) {
  return {
    at_us: at,
    views: catalog.views.map((view, i) => {
      const notable = NOTABLE[view.code];
      return {
        view: view.code,
        snapshot_ts_us: at,
        population: POPULATIONS[i] ?? 0,
        status: "complete",
        notable: notable !== undefined,
        notable_count: notable?.[1] ?? 0,
        notable_level: notable?.[0] ?? "none",
        collection: null,
      };
    }),
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

// --- context ---------------------------------------------------------------

function contextResponse(at) {
  return {
    snapshot_ts_us: at,
    host: {
      boot_id: "5f1c9e2a-7b3d-4c1e-9a2f-ordersdb01",
      kernel_version: "6.8.0-49-generic",
      logical_cpu_count: 32,
      logical_cpu_count_reason: null,
    },
    instance: {
      hostname: "orders-db",
      pg_system_identifier: "7420013589123456789",
      pg_system_identifier_reason: null,
      pg_version_num: 160004,
      role: "primary",
      role_reason: null,
    },
    databases: [
      { entity: "db:16384", name: "orders", oid: 16384, visibility: "full" },
      { entity: "db:16402", name: "billing", oid: 16402, visibility: "full" },
      { entity: "db:5", name: "postgres", oid: 5, visibility: "full" },
    ],
    replication: {
      instance: {
        streaming_replicas: 2,
        timeline_id: 3,
        replay_lag_us: null,
        replay_lag_reason: "not_applicable_on_primary",
      },
      replicas: [
        {
          entity: "replica:orders-db-ro-1",
          application_name: "orders-db-ro-1",
          pid: 22110,
          state: "streaming",
          sync_state: "async",
          replay_lag_us: 900_000,
          replay_lag_reason: null,
        },
        {
          entity: "replica:orders-db-ro-2",
          application_name: "orders-db-ro-2",
          pid: 22111,
          state: "streaming",
          sync_state: "potential",
          replay_lag_us: 2_400_000,
          replay_lag_reason: null,
        },
      ],
    },
    quality: { status: "complete", gaps: [], gated: [], active_tail: true },
  };
}

// --- timeline meta ---------------------------------------------------------

function timelineMeta(fromUs, toUs) {
  return {
    status: "complete",
    fact_set_id: "facts-demo-1",
    response_schema_version: 1,
    view_generation: 1,
    requested_range: { from_us: fromUs, to_us: toUs },
    effective_range: { from_us: fromUs, to_us: toUs },
    effective_step_us: null,
    data_through_us: toUs,
    store_data_through_us: toUs,
    freshness: {
      state: "fresh",
      age_us: "1500000",
      data_through_us: String(toUs),
      expected_period_us: "5000000",
    },
    loss: { dropped_count_lower_bound: null, known_gaps: [] },
    tail_pending: null,
  };
}

// --- spine -----------------------------------------------------------------

const SPINE_GAP = [60, 61]; // two buckets lost to a collector restart
const SPINE_PEAK = 42; // deploy-driven load spike in the middle of the window

function spineSeries(code, unit, aggregation, base, spike, buckets, rand) {
  const phase = rand() * Math.PI * 2;
  const values = [];
  for (let b = 0; b < buckets; b++) {
    if (SPINE_GAP.includes(b)) {
      values.push(null);
      continue;
    }
    const wave = 0.5 + 0.3 * Math.sin((b / buckets) * Math.PI * 4 + phase);
    const peak = 1.8 * Math.exp(-((b - SPINE_PEAK) ** 2) / (2 * 2.5 ** 2));
    const noise = rand() * 0.12;
    values.push(Math.round(base * (wave + noise) + spike * peak));
  }
  return { code, unit, aggregation, values };
}

function spineResponse(params) {
  const from = BigInt(params.get("from") ?? String(nowUs() - 86_400 * US));
  const to = BigInt(params.get("to") ?? String(nowUs()));
  const buckets = Number(params.get("buckets") ?? "96");
  const rand = mulberry32(42);

  const series = [
    spineSeries("load_per_cpu", "ratio", "max", 0.28, 0.9, buckets, rand),
    spineSeries("psi_io_some", "percent", "max", 1.5, 38, buckets, rand),
  ];

  const bucketUs = (to - from) / BigInt(buckets);
  return {
    grid: {
      from_us: from.toString(),
      to_us: to.toString(),
      bucket_count: buckets,
    },
    series,
    quality: {
      status: "partial",
      snapshots: buckets - SPINE_GAP.length,
      gaps: [
        {
          from_us: (from + bucketUs * BigInt(SPINE_GAP[0])).toString(),
          to_us: (
            from +
            bucketUs * BigInt(SPINE_GAP[SPINE_GAP.length - 1] + 1)
          ).toString(),
          reason: "producer_gap",
        },
      ],
      gated: [],
      resource_limited: [],
      active_tail: true,
    },
  };
}

// --- events ----------------------------------------------------------------

function demoEvents(fromUs, toUs) {
  const at = (fraction) => Math.round(fromUs + (toUs - fromUs) * fraction);
  const spec = [
    [0.12, "checkpoint", "info", { kind: "checkpoint" }],
    [0.3, "autovacuum", "info", { kind: "autovacuum" }],
    [0.43, "marker", "info", { kind: "deploy:api-v2.14.0" }],
    [
      0.58,
      "deadlock",
      "deadlock",
      {
        kind: "deadlock",
        category: "deadlock",
        severity: "ERROR",
        sqlstate: "40P01",
        dropped_field_count: 0,
      },
    ],
    [0.74, "checkpoint", "info", { kind: "checkpoint" }],
    [0.9, "autovacuum", "info", { kind: "autovacuum" }],
  ];
  return spec.map(([fraction, kind, cls, payload], i) => {
    const ts = at(fraction);
    return {
      event_id: `demo-${kind}`,
      event_instance_id: `demo-${kind}-${i}`,
      event_kind: kind,
      notable_class: cls,
      sort_ts_us: ts,
      occurred_at_us: ts,
      occurrence_count: 1,
      entity: null,
      payload,
      evidence_quality: "exact",
      identity_quality: "exact",
      quality_flags: 0,
      section_type_id: null,
      observed_interval: null,
      loss: null,
      supporting_evidence: [],
    };
  });
}

function eventsResponse(params) {
  const fromUs = Number(params.get("from") ?? String(nowUs() - 86_400 * US));
  const toUs = Number(params.get("to") ?? String(nowUs()));
  return {
    completeness: "complete",
    retained_exactness: "exact",
    physical_count_semantics: "exact",
    notable_policy_version: 1,
    omitted_by_response_filter: 0,
    events: demoEvents(fromUs, toUs),
    coverage: [],
    next_cursor: null,
    meta: timelineMeta(fromUs, toUs),
  };
}

// --- incidents -------------------------------------------------------------

function incidentsResponse(params) {
  const from = Number(params.get("from") ?? String(nowUs() - 86_400 * US));
  const to = Number(params.get("to") ?? String(nowUs()));
  const span = to - from;
  const at = (fraction) => Math.round(from + span * fraction);

  const lockInterval = { from: at(0.5), to: at(0.66) };
  const vacuumInterval = { from: at(0.7), to: at(0.95) };

  const incidents = [
    {
      incident_key: "inc:orders-db:lock-contention:1",
      interval: lockInterval,
      members: [
        {
          logical_section: "pg_locks",
          identity: ["public.orders"],
          column: "wait_or_hold_us",
          from: lockInterval.from,
          to: lockInterval.to,
        },
        {
          logical_section: "pg_stat_activity",
          identity: [],
          column: "query_duration_us",
          from: at(0.54),
          to: lockInterval.to,
        },
      ],
      findings: [
        {
          lens_id: "lock_contention",
          role: "cause",
          confidence: "high",
          confidence_cap: "high",
          slug: "lock-contention-orders",
          scope: {
            logical_section: "locks",
            identity: ["public.orders"],
            column: "wait_or_hold_us",
          },
          evidence: [
            "wait chain depth 6 on public.orders",
            "blocker pid 12042 idle in transaction 412s",
            "granted AccessExclusiveLock blocks 11 waiters",
          ],
        },
        {
          lens_id: "activity_backlog",
          role: "context",
          confidence: "medium",
          confidence_cap: "high",
          slug: "active-session-backlog",
          scope: {
            logical_section: "activity",
            identity: [],
            column: "query_duration_us",
          },
          evidence: ["142 active sessions", "p95 query duration 38s"],
        },
      ],
      relations: [
        {
          from_finding: 0,
          to_finding: 1,
          kind: "caused",
          provenance: {
            contract: "lock_wait_implies_activity_wait",
            fields: ["pid"],
          },
        },
      ],
      evaluation_complete: true,
      finding_evaluation_status: "complete",
      category_code: "lock_contention",
      coincident_count: 1,
      finding_count: 2,
      level: "critical",
      level_policy_revision: 1,
      peak_ts_us: String(at(0.58)),
      summary_code: "lock_contention_on_orders",
    },
    {
      incident_key: "inc:orders-db:autovacuum-backlog:1",
      interval: vacuumInterval,
      members: [
        {
          logical_section: "pg_stat_user_tables",
          identity: ["public.events"],
          column: "dead_pct",
          from: vacuumInterval.from,
          to: vacuumInterval.to,
        },
      ],
      findings: [
        {
          lens_id: "autovacuum_backlog",
          role: "cause",
          confidence: "medium",
          confidence_cap: "medium",
          slug: "autovacuum-backlog-events",
          scope: {
            logical_section: "tables",
            identity: ["public.events"],
            column: "dead_pct",
          },
          evidence: [
            "dead_pct 18.4% on public.events",
            "last autovacuum 26h ago",
            "modified_since_analyze 4.2M rows",
          ],
        },
      ],
      relations: [],
      evaluation_complete: true,
      finding_evaluation_status: "complete",
      category_code: "maintenance",
      coincident_count: 0,
      finding_count: 1,
      level: "warning",
      level_policy_revision: 1,
      peak_ts_us: String(at(0.82)),
      summary_code: "autovacuum_backlog_events",
    },
  ];

  return {
    from,
    to,
    incidents,
    analysis_status: "complete",
    complete: true,
    clustering_complete: true,
    data_age_seconds: 2,
    catalog: {},
    coverage_by_section: {},
    data_quality: {},
    log: {},
    skipped: {},
  };
}

// --- frame rows (orders-db dataset) ----------------------------------------
// Each generator returns rows shaped {entity, label, data, cls?, cat?} where
// `data` maps view column codes to wire values. Anything missing becomes an
// honest null cell. All timestamps derive from `nowUs()` at request time.

function r2(v) {
  return Math.round(v * 100) / 100;
}

function sparkFor(entity, scale) {
  const rand = mulberry32(hashCode(entity));
  const phase = rand() * Math.PI * 2;
  const values = [];
  for (let i = 0; i < 14; i++) {
    if (i === 9) {
      values.push(null);
      continue;
    }
    const wave = 0.5 + 0.35 * Math.sin((i / 14) * Math.PI * 2 + phase);
    values.push(r2(scale * (wave + rand() * 0.2)));
  }
  return { complete: false, values };
}

function verdict(column, metric, observed, level, boundaryValue) {
  return {
    column,
    metric,
    result: {
      status: "classified",
      level,
      evidence: { kind: "scalar", observed },
      boundary: { operator: ">", value: boundaryValue },
    },
  };
}

const QUERIES = [
  [7101, "UPDATE orders SET status=$1 WHERE id=$2"],
  [7102, "SELECT * FROM sessions WHERE user_id=$1"],
  [7103, "INSERT INTO events (ts,kind,payload) VALUES ($1,$2,$3)"],
  [7104, "SELECT count(*) FROM ledger WHERE account_id=$1"],
  [7105, "DELETE FROM cart_items WHERE expires_at < $1"],
  [7106, "SELECT o.id, o.total FROM orders o JOIN users u ON u.id=o.user_id"],
  [7107, "UPDATE inventory SET qty=qty-$1 WHERE sku=$2"],
  [7108, "SELECT * FROM orders WHERE created_at > $1 ORDER BY id DESC"],
  [7109, "UPDATE sessions SET touched_at=$1 WHERE sid=$2"],
  [7110, "SELECT sum(total) FROM payments WHERE day=$1"],
  [7111, "INSERT INTO audit_log (actor,action,meta) VALUES ($1,$2,$3)"],
  [7112, "VACUUM ANALYZE public.audit_log"],
];

const BACKENDS = [
  ["app", "api-worker", "active"],
  ["app", "api-worker", "idle in transaction"],
  ["app", "api-worker", "active"],
  ["app", "reports", "active"],
  ["etl", "loader", "active"],
  ["app", "api-worker", "idle"],
  ["pgbouncer", "pgbouncer", "idle"],
  ["app", "api-worker", "active"],
  ["app", "api-worker", "idle in transaction"],
  ["etl", "reconciler", "active"],
  ["app", "api-worker", "active"],
  ["postgres", "autovacuum worker", "active"],
  ["postgres", "walwriter", "active"],
  ["app", "api-worker", "active"],
];

const TABLES = [
  // name, seq, idx, dead_pct, dead_tuples, mod_since_analyze, ins_since_vac, av_age_h (null = never)
  ["orders", 412, 9_814_220, 2.1, 41_208, 182_331, 920_112, 3.2],
  ["order_items", 288, 12_404_118, 1.6, 55_014, 220_814, 1_240_551, 3.4],
  ["users", 18, 2_140_502, 0.4, 3_112, 12_410, 48_201, 5.1],
  ["sessions", 96, 18_220_441, 6.8, 412_884, 1_804_112, 3_140_220, 1.2],
  ["ledger", 12, 4_410_208, 0.1, 812, 4_102, 12_884, 9.5],
  ["cart_items", 1_204, 882_114, 12.9, 220_418, 680_114, 940_882, 0.6],
  ["inventory", 44, 1_220_884, 1.1, 8_410, 44_210, 120_441, 4.8],
  ["audit_log", 8_412, 0, 0.0, 0, 1_220_884, 1_220_884, null],
  ["events", 2_884, 1_104_220, 18.4, 1_840_220, 4_210_884, 6_420_118, 26.0],
  ["products", 2, 220_441, 0.2, 88, 1_204, 2_410, 12.2],
  ["payments", 88, 3_310_884, 0.9, 22_410, 88_441, 210_884, 6.4],
  ["shipments", 64, 1_884_220, 1.4, 18_220, 64_884, 140_220, 7.1],
  ["refunds", 8, 220_118, 0.6, 1_204, 8_410, 22_118, 11.8],
  ["job_queue", 412, 440_884, 22.7, 88_441, 220_118, 310_884, 0.4],
  ["sessions_archive", 12_220, 0, 0.0, 0, 88_441, 88_441, null],
];

function rowsActivity() {
  const waitEvents = [
    null,
    "Lock:relation",
    null,
    "IO:DataFileRead",
    null,
    null,
    null,
    "Lock:tuple",
    "Lock:relation",
    null,
    "Client:ClientRead",
    null,
    null,
    "IO:WALWrite",
  ];
  return BACKENDS.map(([user, app, state], i) => {
    const pid = 12041 + i * 13;
    const dur = r2((i + 1) * 2_400_000 + (i % 3) * 880_000);
    const cls = [];
    if (dur > 18_000_000) {
      cls.push(
        verdict(
          "query_duration_us",
          "pg.activity.query_duration_seconds",
          r2(dur / US),
          i % 2 === 0 ? "critical" : "warning",
          10,
        ),
      );
    }
    return {
      entity: `pid:${pid}`,
      label: `${user}/${app} (${state})`,
      data: {
        pid,
        user,
        database:
          user === "postgres" ? "orders" : i % 5 === 4 ? "billing" : "orders",
        application: app,
        state,
        wait_event: waitEvents[i],
        query: QUERIES[i % QUERIES.length][1],
        query_duration_us: state === "idle" ? null : dur,
        transaction_duration_us:
          state === "idle in transaction" ? r2(dur * 3.2) : null,
        cpu: state === "active" ? r2(0.12 + (i % 5) * 0.14) : r2(0.01),
      },
      cls,
    };
  });
}

const STMT_DATABASES = ["orders", "orders", "billing", "analytics"];
const STMT_USERS = ["app_rw", "app_rw", "billing_job", "report"];

function rowsStatements() {
  return QUERIES.map(([qid], i) => {
    const calls = 12_400_000 - i * 912_000;
    const total = r2(8_420_000 - i * 588_000);
    const mean = r2(total / Math.max(calls / 1000, 1));
    const cls = [];
    if (i < 2) {
      cls.push(
        verdict(
          "time_pct",
          "pg.statements.time_pct",
          r2(31.5 - i * 9),
          i === 0 ? "critical" : "warning",
          20,
        ),
      );
    }
    // Mirror the live store: the collector writes the query text as NULL by
    // design, so the label is the bare queryid and identification rides on
    // database/user — the demo must not promise text the stand cannot show.
    const queryid = String(9_180_220_441_120_000n + BigInt(qid));
    return {
      entity: `stmt:${qid}`,
      label: queryid,
      data: {
        queryid,
        query: null,
        database: STMT_DATABASES[i % STMT_DATABASES.length],
        user: STMT_USERS[i % STMT_USERS.length],
        calls,
        total,
        ms_per_row: r2(0.42 + i * 0.18),
        mean,
        time_pct: r2(31.5 - i * 2.6),
        plan_time_pct: i === 7 ? null : r2(1.2 + (i % 4) * 0.8),
        rows: Math.round(calls * (0.8 + (i % 3) * 2.2)),
        hit_pct: r2(99.4 - i * 0.7),
        blks_read: Math.round(420_000 + i * 88_000),
        temp_written: i % 4 === 3 ? Math.round(12_000 + i * 900) : 0,
        wal_bytes: Math.round(8_800_000 - i * 420_000),
      },
      cls,
    };
  });
}

function rowsPlans() {
  const shapes = [
    "Index Scan using orders_pkey on orders",
    "Bitmap Heap Scan on sessions",
    "Seq Scan on events",
    "Nested Loop -> Index Scan on order_items",
    "Hash Join -> Seq Scan on ledger",
    "Index Only Scan using sessions_sid_idx",
    "Sort -> Gather Merge on payments",
    "Append -> Seq Scan on audit_log",
    "Merge Join -> Index Scan on users",
    "GroupAggregate -> Index Scan on inventory",
  ];
  return shapes.map((plan, i) => {
    const planid = 84_102_200 + i * 17_311;
    const cls =
      i === 2
        ? [verdict("mean", "pg.plans.mean_time_us", 48_200, "critical", 10_000)]
        : [];
    return {
      entity: `plan:${planid}`,
      label: plan,
      data: {
        planid: String(planid),
        plan,
        queryid: String(9_180_220_441_120_000n + BigInt(7101 + i)),
        calls: 2_400_000 - i * 188_000,
        mean: r2(48_200 / (i + 1)),
        rows: Math.round(120_000 + i * 44_000),
      },
      cls,
    };
  });
}

function rowsTables() {
  const now = nowUs();
  return TABLES.map(
    ([name, seq, idx, deadPct, dead, modSince, insSince, avAgeH], i) => {
      const cls = [];
      if (deadPct > 10) {
        cls.push(
          verdict(
            "dead_pct",
            "pg.tables.dead_tuple_pct",
            deadPct,
            deadPct > 18 ? "critical" : "warning",
            10,
          ),
        );
      }
      return {
        entity: `table:public.${name}`,
        label: `public.${name}`,
        data: {
          relation: `public.${name}`,
          seq_scan: seq,
          idx_scan: idx,
          dead_pct: deadPct,
          dead_tuples: dead,
          seq_scan_pct: r2((100 * seq) / Math.max(seq + idx, 1)),
          modified_since_analyze: modSince,
          inserted_since_vacuum: insSince,
          last_autovacuum:
            avAgeH === null
              ? null
              : String(now - Math.round(avAgeH * 3600 * US)),
          autovacuum_age_seconds: avAgeH === null ? null : r2(avAgeH * 3600),
          autoanalyze_age_seconds: r2((avAgeH ?? 48) * 3600 * 0.8) + i,
        },
        cls,
      };
    },
  );
}

function rowsIndexes() {
  const defs = [
    ["orders_pkey", "orders", 9_810_220, 1.0],
    ["orders_user_id_idx", "orders", 4_220_884, 3.2],
    ["order_items_order_id_idx", "order_items", 8_410_220, 2.1],
    ["sessions_sid_idx", "sessions", 12_220_441, 1.0],
    ["sessions_user_id_idx", "sessions", 5_884_220, 1.4],
    ["ledger_account_id_idx", "ledger", 4_404_118, 6.8],
    ["cart_items_expires_at_idx", "cart_items", 882_114, 12.4],
    ["inventory_sku_idx", "inventory", 1_220_884, 1.0],
    ["payments_day_idx", "payments", 3_310_884, 420.6],
    ["shipments_order_id_idx", "shipments", 1_884_220, 1.2],
    ["events_ts_idx", "events", 44_102, 88.4],
    ["audit_log_actor_idx", "audit_log", 0, 0.0],
  ];
  return defs.map(([index, table, scans, rps]) => ({
    entity: `index:${index}`,
    label: index,
    data: { index, table, scans, rows_per_scan: rps },
    cls: [],
  }));
}

function rowsVacuum() {
  return [
    ["public.events", "vacuuming heap", true, 0.62, 1_840_220],
    ["public.job_queue", "truncating heap", true, 0.94, 88_441],
    ["public.sessions", "vacuuming indexes", false, 0.41, 412_884],
    ["public.audit_log", "scanning heap", false, 0.18, 0],
  ].map(([table, phase, isAuto, progress, dead], i) => ({
    entity: `pid:${12188 + i * 7}`,
    label: `${isAuto ? "autovacuum" : "vacuum"} ${table}`,
    data: {
      pid: 12188 + i * 7,
      table: String(16390 + i * 12),
      relation: table,
      phase,
      is_autovacuum: isAuto,
      progress,
      dead_tuples: dead,
    },
    cls: [],
  }));
}

function rowsProcesses() {
  const defs = [
    ["postgres: backend api-worker", 0.42, 412_884, 88_220_000, 12_440_000],
    ["postgres: backend api-worker", 0.38, 398_220, 64_880_000, 8_220_000],
    ["postgres: backend reports", 0.31, 884_220, 12_440_000, 220_884],
    ["postgres: backend etl/loader", 0.27, 640_118, 220_884_000, 88_440_000],
    ["postgres: checkpointer", 0.04, 44_220, 8_440_000, 120_884_000],
    ["postgres: background writer", 0.02, 22_884, 0, 44_220_000],
    ["postgres: walwriter", 0.06, 18_441, 0, 88_884_000],
    ["postgres: autovacuum launcher", 0.0, 12_220, 220_884, 0],
    ["postgres: autovacuum worker", 0.18, 220_441, 44_220_000, 4_884_000],
    ["postgres: archiver", 0.01, 8_884, 0, 2_220_000],
    ["postgres: stats collector", 0.01, 14_220, 1_220_000, 440_000],
    ["postgres: logical replication launcher", 0.0, 9_884, 0, 0],
    ["pgbouncer", 0.09, 88_441, 4_440_000, 1_220_000],
    ["node_exporter", 0.02, 22_118, 88_884_000, 12_220_000],
  ];
  return defs.map(([type, cpu, rss, readBps, writeBps], i) => {
    const pid = 12041 + i * 31;
    const cls =
      cpu > 0.4 ? [verdict("cpu", "os.process.cpu", cpu, "warning", 0.4)] : [];
    return {
      entity: `proc:${pid}`,
      label: type,
      data: {
        pid,
        type,
        cpu,
        rss,
        read_bytes_per_second: readBps,
        write_bytes_per_second: writeBps,
        block_delay: r2(i % 4 === 0 ? 0.8 : 0.05),
        command: type,
      },
      cls,
    };
  });
}

function rowsLocks() {
  const defs = [
    [12055, "app / api-worker", "Lock:relation", "public.orders", 412_880_000],
    [12107, "app / api-worker", "Lock:relation", "public.orders", 388_220_000],
    [12120, "app / api-worker", "Lock:tuple", "public.orders", 204_440_000],
    [12133, "etl / loader", "Lock:relation", "public.orders", 188_884_000],
    [12146, "app / api-worker", "Lock:relation", "public.orders", 142_220_000],
    [12159, "app / reports", "Lock:relation", "public.orders", 98_440_000],
    [12172, "app / api-worker", "Lock:transactionid", null, 64_884_000],
    [12185, "etl / reconciler", "Lock:relation", "public.sessions", 42_220_000],
    [12198, "app / api-worker", "Lock:tuple", "public.orders", 18_884_000],
    [12211, "app / api-worker", "Lock:relation", "public.job_queue", 4_220_000],
  ];
  return defs.map(([pid, ua, lock, target, waitUs], i) => {
    const cls =
      waitUs > 120_000_000
        ? [
            verdict(
              "wait_or_hold_us",
              "pg.locks.wait_or_hold_us",
              waitUs,
              i < 2 ? "critical" : "warning",
              60_000_000,
            ),
          ]
        : [];
    return {
      entity: `lock:${pid}`,
      label: `${ua} on ${target ?? "xid"}`,
      data: {
        pid,
        user_application: ua,
        lock,
        target,
        wait_or_hold_us: waitUs,
        query: "UPDATE orders SET status=$1 WHERE id=$2",
      },
      cls,
    };
  });
}

function rowsEventsView() {
  const now = nowUs();
  const defs = [
    [2, "ERROR", 1, null, "deadlock detected: pid 12055 waiting on ShareLock"],
    [5, "LOG", 2, 4_220_000, "checkpoint complete: wrote 41208 buffers"],
    [9, "LOG", 3, 12_880_000, "automatic vacuum of table orders.public.events"],
    [
      14,
      "WARNING",
      4,
      8_440_000,
      "duration: 8440 ms  statement: SELECT count(*) FROM ledger",
    ],
    [
      22,
      "LOG",
      3,
      6_220_000,
      "automatic analyze of table orders.public.sessions",
    ],
    [31, "ERROR", 1, null, "canceling statement due to statement timeout"],
    [38, "LOG", 2, 3_884_000, "checkpoint complete: wrote 38884 buffers"],
    [
      47,
      "WARNING",
      4,
      12_220_000,
      "duration: 12220 ms  statement: SELECT * FROM events",
    ],
    [
      55,
      "LOG",
      3,
      18_440_000,
      "automatic vacuum of table orders.public.job_queue",
    ],
    [
      63,
      "ERROR",
      5,
      null,
      "could not extend file base/16384/16502: No space left on device hint retried ok",
    ],
    [70, "LOG", 2, 4_118_000, "checkpoint complete: wrote 40118 buffers"],
    [
      78,
      "WARNING",
      4,
      6_884_000,
      "duration: 6884 ms  statement: UPDATE inventory SET qty=qty-$1",
    ],
    [
      84,
      "LOG",
      3,
      9_220_000,
      "automatic vacuum of table orders.public.cart_items",
    ],
    [
      90,
      "ERROR",
      1,
      null,
      "duplicate key value violates unique constraint sessions_pkey",
    ],
    [96, "LOG", 2, 5_440_000, "checkpoint complete: wrote 44012 buffers"],
  ];
  return defs.map(([minAgo, severity, type, duration, message], i) => {
    const sevCode =
      severity === "ERROR" ? 21 : severity === "WARNING" ? 17 : 13;
    // Wire codes mirror event_severity_code/event_kind_code in the frame
    // projection: the human text comes from the web i18n dictionaries.
    const severityCode =
      severity === "ERROR"
        ? "error"
        : severity === "WARNING"
          ? "warning"
          : "log";
    const categoryCode =
      type === 1
        ? "pg.log.error_group_observed"
        : type === 2
          ? "pg.checkpoint.completed"
          : type === 4
            ? "pg.query.slow_group_reported"
            : message.includes("analyze")
              ? "pg.maintenance.autoanalyze_reported"
              : "pg.maintenance.autovacuum_reported";
    return {
      entity: `evt:${i + 1}`,
      label: message,
      data: {
        time: String(now - minAgo * 60 * US),
        severity: sevCode,
        severity_code: severityCode,
        type,
        category_code: categoryCode,
        duration,
        message,
      },
      cls:
        severity === "ERROR"
          ? [
              {
                column: "severity_code",
                metric: "pg.events.severity",
                result: {
                  status: "classified",
                  level: "warning",
                  evidence: { kind: "scalar", observed: sevCode },
                },
              },
            ]
          : [],
    };
  });
}

const ROW_GENERATORS = {
  activity: rowsActivity,
  statements: rowsStatements,
  plans: rowsPlans,
  tables: rowsTables,
  indexes: rowsIndexes,
  vacuum: rowsVacuum,
  processes: rowsProcesses,
  locks: rowsLocks,
  events: rowsEventsView,
};

// --- frame -----------------------------------------------------------------

const SPARK_SCALES = {
  activity: 1,
  statements: 1200,
  plans: 48_000,
  tables: 100,
  indexes: 4000,
  vacuum: 1,
  processes: 1,
  locks: 400_000_000,
  events: 5,
};

function compareCells(a, b, order) {
  if (a === null || a === undefined) return 1;
  if (b === null || b === undefined) return -1;
  const na = Number(a);
  const nb = Number(b);
  const cmp =
    !Number.isNaN(na) && !Number.isNaN(nb)
      ? na - nb
      : String(a).localeCompare(String(b));
  return order === "asc" ? cmp : -cmp;
}

function frameResponse(viewCode, params) {
  const view = catalog.views.find((v) => v.code === viewCode);
  if (view === undefined) return null;
  const at = params.get("at") ?? String(nowUs());
  const generate = ROW_GENERATORS[viewCode] ?? (() => []);
  let rows = generate();

  // `q`: minimal substring filter over the row payload.
  const q = params.get("q");
  if (q !== null && q !== "") {
    const needle = q.toLowerCase();
    rows = rows.filter((r) =>
      JSON.stringify(r.data).toLowerCase().includes(needle),
    );
  }
  const matched = rows.length;

  // Stable order: requested sort column first, entity as the tie-break.
  const sort = params.get("sort");
  const order = params.get("order") === "asc" ? "asc" : "desc";
  rows = [...rows].sort((ra, rb) => {
    const cmp = sort ? compareCells(ra.data[sort], rb.data[sort], order) : 0;
    return cmp !== 0 ? cmp : ra.entity.localeCompare(rb.entity);
  });

  const limit = Math.max(1, Number(params.get("limit") ?? "100"));
  const cursorParam = params.get("cursor");
  const offset =
    cursorParam !== null && /^o:\d+$/.test(cursorParam)
      ? Number(cursorParam.slice(2))
      : 0;
  const pageRows = rows.slice(offset, offset + limit);
  const next = offset + limit < matched ? `o:${offset + limit}` : null;

  // Mirror the backend admission: a preset (default = first) selects the
  // frame columns, lazy columns never ride the frame.
  const presetParam = params.get("preset");
  const preset = presetParam
    ? view.presets.find((p) => p.code === presetParam)
    : view.presets[0];
  const frameColumns = (preset?.columns ?? view.columns.map((c) => c.code))
    .map((code) => view.columns.find((c) => c.code === code && !c.lazy))
    .filter((c) => c !== undefined);
  const columns = frameColumns.map((c) => ({
    code: c.code,
    type: c.type,
    hidden: false,
    threshold_metric: c.threshold_metric ?? null,
    unit: c.unit ?? null,
  }));

  return {
    view: viewCode,
    snapshot_ts_us: at,
    columns,
    rows: pageRows.map((r) => ({
      entity: r.entity,
      label: r.label,
      cells: frameColumns.map((c) => r.data[c.code] ?? null),
      classifications: r.cls ?? [],
      spark: sparkFor(r.entity, SPARK_SCALES[viewCode] ?? 100),
    })),
    page: { matched, returned: pageRows.length, next },
    neighbors: {},
    quality: {
      status: "complete",
      snapshots: 42,
      gaps: [],
      gated: [],
      unavailable_revision: [],
      resource_limited: [],
      active_tail: true,
    },
  };
}

// --- entity detail ---------------------------------------------------------

function entityResponse(viewCode, entity, params) {
  const view = catalog.views.find((v) => v.code === viewCode);
  if (view === undefined) return null;
  const generate = ROW_GENERATORS[viewCode] ?? (() => []);
  const row = generate().find((r) => r.entity === entity);
  if (row === undefined) return null;

  const at = params.get("at");
  if (at !== null) {
    return {
      view: viewCode,
      entity,
      label: row.label,
      mode: "point",
      snapshot_ts_us: at,
      fields: view.columns.map((c) => {
        const value = row.data[c.code] ?? null;
        return {
          code: c.code,
          value,
          status: value === null ? "not_collected" : "available",
          reason: value === null ? "not_collected" : null,
        };
      }),
      related: [],
      quality: { status: "complete", gaps: [], gated: [] },
    };
  }

  // History mode: follow the entity over the trailing hour in 5-minute steps.
  const now = nowUs();
  const columns = view.columns
    .filter((c) => !c.lazy)
    .slice(0, 5)
    .map((c) => c.code);
  const rand = mulberry32(hashCode(entity));
  const snapshots = [];
  for (let i = 11; i >= 0; i--) {
    snapshots.push({
      ts_us: String(now - i * 300 * US),
      values: columns.map((code) => {
        const value = row.data[code] ?? null;
        if (typeof value === "number") return r2(value * (0.8 + rand() * 0.4));
        return value;
      }),
    });
  }
  return {
    view: viewCode,
    entity,
    label: row.label,
    mode: "history",
    columns,
    snapshots,
    page: { next: null },
    quality: { status: "complete", gaps: [], gated: [] },
  };
}

// --- data quality ----------------------------------------------------------

function dataQualityResponse(params) {
  const to = Number(params.get("to") ?? String(nowUs()));
  return {
    status: "partial",
    freshness: {
      state: "fresh",
      age_us: "1800000",
      data_through_us: String(to),
      expected_period_us: "5000000",
    },
    coverage: {
      complete_snapshots: 42,
      expected_snapshots: 45,
      observed_snapshots: 44,
    },
    gaps: [
      {
        from_us: String(to - 6 * 3600 * US),
        to_us: String(to - 6 * 3600 * US + 600 * US),
        reason: "producer_restart",
      },
    ],
    capabilities: [
      ...catalog.views.map((view) => ({
        code: view.code,
        kind: "projection",
        status: view.availability === "available" ? "available" : "unavailable",
        reason: view.availability === "available" ? null : "not_collected",
      })),
      { code: "lock_chain", kind: "lens", status: "available", reason: null },
      {
        code: "autovacuum_backlog",
        kind: "lens",
        status: "available",
        reason: null,
      },
      {
        code: "plan_regression",
        kind: "lens",
        status: "unavailable",
        reason: "not_collected",
      },
    ],
    integrity: {
      status: "complete",
      corrupt_segments: 0,
      last_catalog_refresh_us: String(to - 60 * US),
      quarantined_entries: 0,
      readable_segments: 36,
    },
    producer: {
      state: "running",
      collector_pid: 12002,
      collector_started_at_us: String(to - 14 * 3600 * US),
      last_status_at_us: String(to - 30 * US),
    },
    quality: { status: "partial", resource_limited: [], active_tail: true },
  };
}

// --- storage ---------------------------------------------------------------

const GIB = 1024 ** 3;

function storageResponse() {
  const total = 500 * GIB;
  const available = 318 * GIB;
  return {
    filesystem: {
      available_bytes: available,
      total_bytes: total,
      used_fraction: r2(1 - available / total),
    },
    forecast: {
      full_in_days: 212.4,
      full_in_days_reason: null,
      window_us: "604800000000",
      write_rate_bytes_per_day: 1_540_000_000,
    },
    integrity: {
      orphan_overviews: 0,
      quarantined_entries: 0,
      readable_segments: 36,
    },
    quality: { status: "complete", gated: [] },
    retention: {
      status: "known",
      configured_limit: 40 * GIB,
      effective_limit_bytes: 40 * GIB,
      mode: "fixed_bytes",
      reason: null,
    },
    used_bytes: {
      journal: 220 * 1024 ** 2,
      other: 18 * 1024 ** 2,
      ovf: 4 * GIB,
      pgm: 27 * GIB,
      quarantine: 0,
    },
  };
}

// --- heatmap ---------------------------------------------------------------

// Heatmap entities are derived from the same row generators the entity
// endpoint resolves through — every heatmap row is drillable by construction
// (no hand-maintained pid/token lists to drift apart).
const ENTITIES = Object.fromEntries(
  Object.entries(ROW_GENERATORS).map(([view, generate]) => [
    view,
    generate().map((row) => [row.entity, row.label]),
  ]),
);

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

function heatmapResponse(params) {
  const view = params.get("view") ?? "statements";
  const metric = params.get("metric") ?? "time";
  const from = BigInt(params.get("from") ?? "0");
  const to = BigInt(params.get("to") ?? "86400000000");
  const buckets = Number(params.get("buckets") ?? "56");
  const top = Number(params.get("top") ?? "8");

  const rand = mulberry32(42);
  const style = METRIC_STYLE[metric] ?? { unit: "count", scale: 1000 };
  const entities = (ENTITIES[view] ?? []).slice(0, top);

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

function sendJson(res, body, status = 200) {
  const payload = JSON.stringify(body);
  res.writeHead(status, {
    "content-type": "application/json",
    "content-length": Buffer.byteLength(payload),
  });
  res.end(payload);
}

function sendError(res, status, code) {
  sendJson(res, { code, params: {} }, status);
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
  const params = url.searchParams;

  if (url.pathname === "/v1/ui/catalog") return sendJson(res, catalog);
  if (url.pathname === "/v1/views/summary") {
    return sendJson(res, summaryResponse(params.get("at") ?? String(nowUs())));
  }
  if (url.pathname === "/v1/ui/context") {
    return sendJson(res, contextResponse(params.get("at") ?? String(nowUs())));
  }
  if (url.pathname === "/v1/timeline/spine") {
    return sendJson(res, spineResponse(params));
  }
  if (url.pathname === "/v1/timeline/events") {
    return sendJson(res, eventsResponse(params));
  }
  if (url.pathname === "/v1/timeline/heatmap") {
    return sendJson(res, heatmapResponse(params));
  }
  if (url.pathname === "/v1/incidents") {
    return sendJson(res, incidentsResponse(params));
  }
  if (url.pathname === "/v1/data/quality") {
    return sendJson(res, dataQualityResponse(params));
  }
  if (url.pathname === "/v1/storage") {
    return sendJson(res, storageResponse());
  }

  const frameMatch = url.pathname.match(/^\/v1\/frame\/([^/]+)$/);
  if (frameMatch !== null) {
    const body = frameResponse(decodeURIComponent(frameMatch[1]), params);
    if (body === null) return sendError(res, 410, "view_gone");
    return sendJson(res, body);
  }

  const entityMatch = url.pathname.match(/^\/v1\/entity\/([^/]+)\/(.+)$/);
  if (entityMatch !== null) {
    // Mirror the backend's admission: point (`at`) or history
    // (`from`+`to`+`columns`) — a bare token is a 400, not a guess.
    const pointShape = params.get("at") !== null;
    const historyShape =
      params.get("from") !== null &&
      params.get("to") !== null &&
      params.get("columns") !== null;
    if (!pointShape && !historyShape) {
      return sendError(res, 400, "invalid_query_constraint");
    }
    const body = entityResponse(
      decodeURIComponent(entityMatch[1]),
      decodeURIComponent(entityMatch[2]),
      params,
    );
    if (body === null) return sendError(res, 404, "entity_not_found");
    return sendJson(res, body);
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
