import { useTranslation } from "react-i18next";
import { useTimeGeometry } from "../state/timeGeometry";
import { Spine } from "./Spine";

/** Public persistent PG + OS aggregate. Time interaction belongs exclusively
 * to TimeGeometryProvider; Spine keeps the established query/render backend. */
export function HealthLine() {
  const { t } = useTranslation();
  const {
    state,
    range,
    hoverUs,
    brushDraft,
    setCursor,
    setHover,
    setBrushDraft,
    commitRange,
    setBaseline,
  } = useTimeGeometry();

  return (
    <Spine
      at={state.at}
      span={state.span}
      baseline={state.baseline}
      range={range}
      hoverUs={hoverUs}
      brushDraft={brushDraft}
      onSelectAt={setCursor}
      onSelectBaseline={setBaseline}
      onHover={setHover}
      onBrushDraft={setBrushDraft}
      onCommitRange={commitRange}
      meaning={t("healthLine.coincidence")}
      showProvenance
    />
  );
}
