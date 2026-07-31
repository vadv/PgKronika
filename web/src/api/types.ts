// Hand-written aliases over the generated OpenAPI schema. This module is
// the single import point for API types; add an alias here instead of
// importing `schema.d.ts` from components. Names follow the spec, not
// legacy local names (`ViewSummaryResponse`, not `SummaryResponse`).
import type { components } from "./schema";

export type Availability = components["schemas"]["Availability"];
export type Scope = components["schemas"]["Scope"];
export type ValueType = components["schemas"]["ValueType"];
export type MetricSpec = components["schemas"]["MetricSpec"];
export type ColumnSpec = components["schemas"]["ColumnSpec"];
export type PresetSpec = components["schemas"]["PresetSpec"];
export type ViewSpec = components["schemas"]["ViewSpec"];
export type ProjectionCatalog = components["schemas"]["ProjectionCatalog"];
export type SummaryQuality = components["schemas"]["SummaryQuality"];
export type HeatmapQuality = components["schemas"]["HeatmapQuality"];
export type ViewSummaryItem = components["schemas"]["ViewSummaryItem"];
export type ViewSummaryResponse = components["schemas"]["ViewSummaryResponse"];
export type HeatmapRow = components["schemas"]["HeatmapRow"];
export type HeatmapResponse = components["schemas"]["HeatmapResponse"];
export type VersionResponse = components["schemas"]["VersionResponse"];
