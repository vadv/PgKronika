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
export type FrameValue = components["schemas"]["FrameValue"];
export type ClassificationResultDto =
  components["schemas"]["ClassificationResultDto"];
export type CellClassificationDto =
  components["schemas"]["CellClassificationDto"];
export type FrameColumnDto = components["schemas"]["FrameColumnDto"];
export type FrameNeighborsDto = components["schemas"]["FrameNeighborsDto"];
export type FramePageDto = components["schemas"]["FramePageDto"];
export type FrameQualityDto = components["schemas"]["FrameQualityDto"];
export type FrameRowDto = components["schemas"]["FrameRowDto"];
export type FrameResponse = components["schemas"]["FrameResponse"];
export type TimelineMetaDto = components["schemas"]["TimelineMetaDto"];
export type HealthPointResponse = components["schemas"]["HealthPointResponse"];
export type HealthResponse = components["schemas"]["HealthResponse"];
export type EventFact = components["schemas"]["EventFact"];
export type EventsResponseDto = components["schemas"]["EventsResponseDto"];
export type IncidentFindingResponse =
  components["schemas"]["IncidentFindingResponse"];
export type IncidentIntervalResponse =
  components["schemas"]["IncidentIntervalResponse"];
export type IncidentMemberResponse =
  components["schemas"]["IncidentMemberResponse"];
export type IncidentResponse = components["schemas"]["IncidentResponse"];
export type IncidentsResponse = components["schemas"]["IncidentsResponse"];
