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
export type EvidenceDto = components["schemas"]["EvidenceDto"];
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
export type SparkDto = components["schemas"]["SparkDto"];
export type ContextResponse = components["schemas"]["ContextResponse"];
export type SpineResponse = components["schemas"]["SpineResponse"];
export type SpineSeries = components["schemas"]["SpineSeries"];
export type SpineQuality = components["schemas"]["SpineQuality"];
export type DataQualityResponse = components["schemas"]["DataQualityResponse"];
export type CapabilityDto = components["schemas"]["CapabilityDto"];
export type CoverageDto = components["schemas"]["CoverageDto"];
export type FreshnessDto = components["schemas"]["FreshnessDto"];
export type IntegrityDto = components["schemas"]["DataQualityIntegrityDto"];
export type ProducerDto = components["schemas"]["ProducerDto"];
export type QualityDto = components["schemas"]["DataQualityMetaDto"];
export type QualityGap = components["schemas"]["QualityGap"];
export type StorageResponse = components["schemas"]["StorageResponse"];
export type FilesystemDto = components["schemas"]["FilesystemDto"];
export type ForecastDto = components["schemas"]["ForecastDto"];
export type RetentionDto = components["schemas"]["RetentionDto"];
export type UsedBytesDto = components["schemas"]["UsedBytesDto"];
export type EntityResponse = components["schemas"]["EntityResponse"];
export type EntityPointResponse = components["schemas"]["EntityPointResponse"];
export type EntityHistoryResponse =
  components["schemas"]["EntityHistoryResponse"];
export type EntityFieldDto = components["schemas"]["EntityFieldDto"];
export type EntitySnapshotDto = components["schemas"]["EntitySnapshotDto"];
export type EntityQualityDto = components["schemas"]["EntityQualityDto"];
export type RelatedEntityDto = components["schemas"]["RelatedEntityDto"];
