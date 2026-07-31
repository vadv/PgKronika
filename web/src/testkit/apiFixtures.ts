import type {
  ContextResponse,
  DataQualityResponse,
  EntityHistoryResponse,
  EntityPointResponse,
  EventFact,
  EventsResponseDto,
  FrameColumnDto,
  FrameResponse,
  FrameRowDto,
  HealthPointResponse,
  HealthResponse,
  HeatmapQuality,
  IncidentFindingResponse,
  IncidentResponse,
  IncidentsResponse,
  MetricSpec,
  SpineResponse,
  StorageResponse,
  SummaryQuality,
  TimelineMetaDto,
  ViewSpec,
  ViewSummaryItem,
  ViewSummaryResponse,
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
    capabilities: {
      detail: false,
      history: false,
      related: false,
    },
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

export function makeViewSummaryItem(
  overrides: Partial<ViewSummaryItem> = {},
): ViewSummaryItem {
  return {
    view: "activity",
    snapshot_ts_us: "1722400000000000",
    population: 0,
    status: "complete",
    notable: false,
    notable_count: 0,
    notable_level: "none",
    collection: null,
    ...overrides,
  };
}

export function makeViewSummaryResponse(
  overrides: Partial<ViewSummaryResponse> = {},
): ViewSummaryResponse {
  return {
    at_us: "1722400000000000",
    quality: makeSummaryQuality(),
    views: [],
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

export function makeFrameColumn(
  overrides: Partial<FrameColumnDto> = {},
): FrameColumnDto {
  return {
    code: "column",
    type: "i64",
    hidden: false,
    ...overrides,
  };
}

export function makeFrameRow(
  overrides: Partial<FrameRowDto> = {},
): FrameRowDto {
  return {
    entity: "db:1",
    label: "postgres",
    cells: [],
    classifications: [],
    categorical_classifications: [],
    cell_statuses: [],
    spark: { complete: true, values: [] },
    ...overrides,
  };
}

export function makeFrameResponse(
  overrides: Partial<FrameResponse> = {},
): FrameResponse {
  return {
    view: "activity",
    snapshot_ts_us: "1722400000000000",
    columns: [makeFrameColumn()],
    rows: [],
    page: { matched: 0, returned: 0 },
    neighbors: {},
    quality: {
      status: "complete",
      snapshots: 0,
      gaps: [],
      gated: [],
      unavailable_revision: [],
      resource_limited: [],
      active_tail: false,
    },
    ...overrides,
  };
}

export function makeTimelineMeta(
  overrides: Partial<TimelineMetaDto> = {},
): TimelineMetaDto {
  return {
    status: "complete",
    fact_set_id: "facts-1",
    response_schema_version: 1,
    view_generation: 1,
    requested_range: { from_us: 0, to_us: 3_600_000_000 },
    effective_range: { from_us: 0, to_us: 3_600_000_000 },
    effective_step_us: null,
    data_through_us: null,
    store_data_through_us: null,
    freshness: {
      status: "fresh",
      completeness: "complete",
      data_through_us: null,
      physical_count_semantics: "exact",
      retained_exactness: "exact",
    },
    loss: { dropped_count_lower_bound: null, known_gaps: [] },
    tail_pending: null,
    ...overrides,
  };
}

export function makeHealthPoint(
  overrides: Partial<HealthPointResponse> = {},
): HealthPointResponse {
  return {
    interval: { from_us: 0, to_us: 60_000_000 },
    factor_set_id: "factors-1",
    health_policy_version: 1,
    overall_state: "ok",
    overall_score: 1,
    continuous_score: 1,
    domains: [],
    coverage: [],
    floor_evidence: [],
    ...overrides,
  };
}

export function makeHealthResponse(
  overrides: Partial<HealthResponse> = {},
): HealthResponse {
  return {
    health_policy_version: 1,
    factor_set_ids: ["factors-1"],
    points: [makeHealthPoint()],
    coverage: [],
    meta: makeTimelineMeta(),
    ...overrides,
  };
}

export function makeEventFact(overrides: Partial<EventFact> = {}): EventFact {
  return {
    event_id: "event-1",
    event_instance_id: "instance-1",
    event_kind: "marker",
    notable_class: "info",
    sort_ts_us: 1_722_400_000_000_000,
    occurred_at_us: 1_722_400_000_000_000,
    occurrence_count: 1,
    entity: null,
    payload: { kind: "marker" },
    evidence_quality: "exact",
    identity_quality: "exact",
    quality_flags: 0,
    section_type_id: null,
    observed_interval: null,
    loss: null,
    supporting_evidence: [],
    ...overrides,
  };
}

export function makeEventsResponse(
  overrides: Partial<EventsResponseDto> = {},
): EventsResponseDto {
  return {
    completeness: "complete",
    retained_exactness: "exact",
    physical_count_semantics: "exact",
    notable_policy_version: 1,
    omitted_by_response_filter: 0,
    events: [],
    coverage: [],
    next_cursor: null,
    meta: makeTimelineMeta(),
    ...overrides,
  };
}

export function makeIncidentFinding(
  overrides: Partial<IncidentFindingResponse> = {},
): IncidentFindingResponse {
  return {
    lens_id: "lens-1",
    role: "cause",
    confidence: "high",
    confidence_cap: "high",
    slug: "finding-1",
    scope: {
      logical_section: "pg_stat_database",
      identity: [],
      column: "xact",
    },
    evidence: [],
    ...overrides,
  };
}

export function makeIncident(
  overrides: Partial<IncidentResponse> = {},
): IncidentResponse {
  return {
    incident_key: "incident-1",
    interval: { from: 0, to: 3_600_000_000 },
    members: [],
    findings: [],
    relations: [],
    evaluation_complete: true,
    finding_evaluation_status: "complete",
    category_code: "uncategorized",
    coincident_count: 0,
    finding_count: 0,
    level: "info",
    level_policy_revision: 1,
    peak_ts_us: "1722400000000000",
    summary_code: "summary",
    ...overrides,
  };
}

export function makeIncidentsResponse(
  overrides: Partial<IncidentsResponse> = {},
): IncidentsResponse {
  return {
    from: 0,
    to: 3_600_000_000,
    incidents: [],
    analysis_status: "complete",
    complete: true,
    clustering_complete: true,
    data_age_seconds: null,
    catalog: {},
    coverage_by_section: {},
    data_quality: {},
    log: {},
    skipped: {},
    ...overrides,
  };
}

export function makeContextResponse(
  overrides: Partial<ContextResponse> = {},
): ContextResponse {
  return {
    snapshot_ts_us: "1722400000000000",
    host: {},
    instance: {},
    databases: [],
    replication: { replicas: [] },
    quality: { status: "complete", gaps: [], gated: [], active_tail: false },
    ...overrides,
  };
}

export function makeSpineResponse(
  overrides: Partial<SpineResponse> = {},
): SpineResponse {
  return {
    grid: { from_us: "0", to_us: "3600000000", bucket_count: 60 },
    series: [],
    quality: {
      status: "complete",
      snapshots: 0,
      gaps: [],
      gated: [],
      resource_limited: [],
      active_tail: false,
    },
    ...overrides,
  };
}

export function makeDataQualityResponse(
  overrides: Partial<DataQualityResponse> = {},
): DataQualityResponse {
  return {
    status: "fresh",
    freshness: {
      state: "fresh",
      age_us: null,
      data_through_us: null,
      expected_period_us: null,
    },
    coverage: {
      complete_snapshots: 0,
      expected_snapshots: null,
      observed_snapshots: 0,
    },
    gaps: [],
    capabilities: [],
    integrity: {
      status: "complete",
      corrupt_segments: 0,
      last_catalog_refresh_us: null,
      quarantined_entries: 0,
      readable_segments: 0,
    },
    producer: {
      state: "running",
      collector_pid: null,
      collector_started_at_us: null,
      last_status_at_us: null,
    },
    quality: { status: "complete", resource_limited: [], active_tail: false },
    ...overrides,
  };
}

export function makeStorageResponse(
  overrides: Partial<StorageResponse> = {},
): StorageResponse {
  return {
    filesystem: { available_bytes: 0, total_bytes: 0, used_fraction: 0 },
    forecast: {
      full_in_days: null,
      full_in_days_reason: null,
      window_us: "0",
      write_rate_bytes_per_day: null,
    },
    integrity: {
      orphan_overviews: 0,
      quarantined_entries: 0,
      readable_segments: 0,
    },
    quality: { status: "complete", gated: [] },
    retention: {
      status: "ok",
      configured_limit: null,
      effective_limit_bytes: null,
      mode: null,
      reason: null,
    },
    used_bytes: { journal: 0, other: 0, ovf: 0, pgm: 0, quarantine: 0 },
    ...overrides,
  };
}

export function makeEntityPointResponse(
  overrides: Partial<EntityPointResponse> = {},
): EntityPointResponse {
  return {
    view: "activity",
    entity: "db:1",
    mode: "point",
    snapshot_ts_us: "1722400000000000",
    fields: [],
    related: [],
    quality: { status: "complete", gaps: [], gated: [] },
    ...overrides,
  };
}

export function makeEntityHistoryResponse(
  overrides: Partial<EntityHistoryResponse> = {},
): EntityHistoryResponse {
  return {
    view: "activity",
    entity: "db:1",
    mode: "history",
    columns: [],
    snapshots: [],
    page: { next: null },
    quality: { status: "complete", gaps: [], gated: [] },
    ...overrides,
  };
}
