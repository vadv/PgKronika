export type Availability =
  | "available"
  | "gated"
  | "not_collected"
  | "unsupported_type";
export type Scope = "database" | "host" | "instance";
export type ValueType =
  | "i64" | "u64" | "f64" | "bool" | "text" | "timestamp";

export interface MetricSpec {
  code: string;
  revision: number;
  unit: string;
  aggregation: string;
  formula: string;
  requires: string[];
  availability: Availability;
}

export interface ColumnSpec {
  code: string;
  type: ValueType;
  source?: string;
  formula?: string;
  unit?: string;
  threshold_metric?: string;
  lazy: boolean;
  requires: string[];
  availability: Availability;
}

export interface PresetSpec {
  code: string;
  columns: string[];
  sort: { column: string; order: "asc" | "desc" };
}

export interface ViewSpec {
  view_code: number;
  code: string;
  view_revision: number;
  scope: Scope;
  identity_revision: number;
  availability: Availability;
  inputs: unknown[];
  joins: unknown[];
  metrics: MetricSpec[];
  columns: ColumnSpec[];
  presets: PresetSpec[];
  canonical_metric: string;
}

export interface ProjectionCatalog {
  revision: number;
  views: ViewSpec[];
}

export interface QualityMeta {
  status: "complete" | "partial" | "unavailable";
  snapshots: number;
  gaps: { from_us: string; to_us: string }[];
  gated: string[];
  unavailable_revision: string[];
  resource_limited: string[];
  active_tail?: boolean;
}

export interface ViewSummaryItem {
  view: string;
  snapshot_ts_us: string | null;
  population: number | null;
  status: string;
  notable: boolean;
}

export interface SummaryResponse {
  at_us: string;
  views: ViewSummaryItem[];
  quality: QualityMeta;
}

export interface HeatmapRow {
  entity: string;
  label: string;
  unit: string;
  score: { lower: number; upper: number };
  values: (number | null)[];
}

export interface HeatmapResponse {
  grid: { from_us: string; to_us: string; bucket_count: number };
  ranking: { exact: boolean; unseen_upper: number };
  rows: HeatmapRow[];
  quality: QualityMeta & { unbounded_segments?: string[] };
}
