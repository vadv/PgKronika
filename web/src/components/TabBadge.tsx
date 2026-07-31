export function TabBadge(props: {
  population: number | null;
  status: string;
  notable: boolean;
}) {
  return (
    <span
      data-notable={props.notable}
      style={{
        fontFamily: "var(--mono-font)",
        fontSize: "0.85em",
        color: props.notable
          ? "var(--sev-warn)"
          : props.status === "complete"
            ? "var(--fg-dim)"
            : "var(--sev-crit)",
        marginInlineStart: "4px",
      }}
    >
      {props.population ?? "—"}
    </span>
  );
}
