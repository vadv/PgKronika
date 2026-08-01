export function TabBadge(props: {
  population: number | null;
  status: string;
  notable: boolean;
}) {
  // No dangling dash: the counter appears only when a real count exists; a
  // notable view without a population still gets its warning mark.
  if (props.population === null && !props.notable) return null;
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
      {props.population ?? "!"}
    </span>
  );
}
