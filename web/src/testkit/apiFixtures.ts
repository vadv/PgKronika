import type {
  HeatmapQuality,
  MetricSpec,
  SummaryQuality,
  ViewSpec,
} from "../api/types";

/**
 * Minimal spec-valid builders for tests. Every required field of the
 * generated OpenAPI type is present, so a fixture drifts out of date the
 * moment the spec changes (typecheck fails), unlike `as unknown as T`
 * casts. Test-only module — do not import from production code.
 */

export function makeMetricSpec(
  overrides: Partial<MetricSpec> = {},
): MetricSpec {
  return {
    code: "metric",
    revision: 1,
    unit: "ms",
    aggregation: "avg",
    formula: "a / b",
    requires: [],
    availability: "available",
    ...overrides,
  };
}

export function makeViewSpec(overrides: Partial<ViewSpec> = {}): ViewSpec {
  return {
    code: "view",
    view_code: 1,
    view_revision: 1,
    scope: "database",
    identity_revision: 1,
    availability: "available",
    inputs: [],
    joins: [],
    metrics: [],
    columns: [],
    presets: [],
    canonical_metric: "metric",
    ...overrides,
  };
}

export function makeSummaryQuality(
  overrides: Partial<SummaryQuality> = {},
): SummaryQuality {
  return {
    status: "complete",
    snapshots: 0,
    gaps: [],
    gated: [],
    unavailable_revision: [],
    resource_limited: [],
    active_tail: false,
    ...overrides,
  };
}

export function makeHeatmapQuality(
  overrides: Partial<HeatmapQuality> = {},
): HeatmapQuality {
  return {
    status: "complete",
    snapshots: 0,
    gaps: [],
    gated: [],
    unavailable_revision: [],
    resource_limited: [],
    active_tail: false,
    unbounded_segments: [],
    ...overrides,
  };
}
