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
