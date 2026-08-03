import { useCallback, useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import type { ViewSpec } from "../api/types";
import {
  compileForensicSearch,
  type ForensicSearchError,
  type ForensicSearchGroup as SearchGroupPlan,
} from "../search/compile";
import { useForensicSearchGroup } from "../search/group";
import { formatIntervalTime } from "./FocusBar";

export interface ForensicSearchProps {
  open: boolean;
  views: ViewSpec[];
  at: string;
  span: number;
  onClose: () => void;
  onSelect: (view: string, entity: string) => void;
}

function errorText(
  error: ForensicSearchError,
  t: (key: string, options?: Record<string, unknown>) => string,
): string {
  if (error.code === "unsupported_key") {
    return t("search.error.unsupportedKey", {
      key: error.key,
      defaultValue: `${error.key}: no public searchable evidence field`,
    });
  }
  if (error.code === "query_too_long") {
    return t("search.error.queryTooLong", {
      limit: error.limit,
      defaultValue: `Search is limited to ${error.limit} UTF-8 bytes`,
    });
  }
  if (error.code === "too_many_terms") {
    return t("search.error.tooManyTerms", {
      limit: error.limit,
      defaultValue: `Search is limited to ${error.limit} AND terms`,
    });
  }
  return t("search.error.invalid", { defaultValue: "Invalid search syntax" });
}

function SearchResultGroup(props: {
  groupKey: string;
  plan: SearchGroupPlan;
  at: string;
  span: number;
  onSelect: (view: string, entity: string) => void;
  onResultKeyDown: React.KeyboardEventHandler<HTMLButtonElement>;
  onStatus: (
    key: string,
    status: { state: "pending" | "success" | "error"; matched: number },
  ) => void;
}) {
  const { t } = useTranslation();
  const search = useForensicSearchGroup({
    view: props.plan.view.code,
    at: props.at,
    span: props.span,
    q: props.plan.q,
    enabled: true,
  });
  const status = search.isPending
    ? "pending"
    : search.isError
      ? "error"
      : "success";
  const reportStatus = props.onStatus;
  const statusKey = props.groupKey;
  useEffect(() => {
    reportStatus(statusKey, { state: status, matched: search.matched });
  }, [reportStatus, search.matched, status, statusKey]);
  if (!search.isPending && !search.isError && search.matched === 0) return null;
  return (
    <section
      data-search-group={props.plan.view.code}
      style={{
        borderBlockStart: "1px solid var(--border)",
        paddingBlock: "var(--space-2)",
      }}
    >
      <div
        style={{
          display: "flex",
          alignItems: "baseline",
          gap: "var(--space-2)",
          paddingInline: "var(--space-2)",
          marginBlockEnd: "var(--space-1)",
        }}
      >
        <strong style={{ fontFamily: "var(--ui-font)" }}>
          {t(`tabs.${props.plan.view.code}`, {
            defaultValue: props.plan.view.code,
          })}
        </strong>
        <span
          style={{
            fontFamily: "var(--mono-font)",
            color: "var(--fg-dim)",
            fontSize: "var(--text-xs)",
          }}
        >
          {search.isPending
            ? t("search.searching", { defaultValue: "searching…" })
            : t("search.count", {
                shown: search.rows.length,
                matched: search.matched,
                defaultValue: `${search.rows.length} / ${search.matched}`,
              })}
        </span>
        <span
          style={{
            marginInlineStart: "auto",
            color: "var(--fg-dim)",
            fontFamily: "var(--mono-font)",
            fontSize: "var(--text-xs)",
          }}
        >
          {props.plan.reason}
        </span>
      </div>
      {search.isError && (
        <div role="alert" style={{ color: "var(--sev-warn-fg)" }}>
          {t("search.error.group", { defaultValue: "Search group failed" })}
        </div>
      )}
      {search.rows.map((row) => (
        <button
          key={row.entity}
          type="button"
          data-search-result
          onClick={() => props.onSelect(props.plan.view.code, row.entity)}
          onKeyDown={props.onResultKeyDown}
          style={{
            display: "grid",
            gridTemplateColumns: "minmax(220px, 1fr) minmax(160px, 0.7fr)",
            gap: "var(--space-3)",
            width: "100%",
            minHeight: "44px",
            padding: "var(--space-2)",
            color: "var(--fg)",
            background: "transparent",
            border: "none",
            borderRadius: "var(--radius-sm)",
            textAlign: "start",
            cursor: "pointer",
          }}
        >
          <span style={{ minWidth: 0 }}>
            <span
              style={{
                display: "block",
                overflow: "hidden",
                textOverflow: "ellipsis",
                whiteSpace: "nowrap",
                fontFamily: "var(--ui-font)",
                fontWeight: 600,
              }}
            >
              {row.label}
            </span>
            <span
              style={{
                display: "block",
                overflow: "hidden",
                textOverflow: "ellipsis",
                whiteSpace: "nowrap",
                color: "var(--fg-dim)",
                fontFamily: "var(--mono-font)",
                fontSize: "var(--text-xs)",
              }}
            >
              {row.cells
                .filter((value) => value !== null)
                .slice(0, 3)
                .map(String)
                .join(" · ")}
            </span>
          </span>
          <span
            style={{
              minWidth: 0,
              color: "var(--fg-dim)",
              fontFamily: "var(--mono-font)",
              fontSize: "var(--text-xs)",
              overflow: "hidden",
              textOverflow: "ellipsis",
              whiteSpace: "nowrap",
            }}
          >
            frame:{props.plan.view.code}
            {search.snapshotTsUs !== null
              ? ` · ${formatIntervalTime(Number(search.snapshotTsUs))}`
              : ""}
            {search.quality !== undefined ? ` · ${search.quality.status}` : ""}
          </span>
        </button>
      ))}
      {search.hasNextPage && (
        <button
          type="button"
          disabled={search.isFetchingNextPage}
          onClick={() => void search.fetchNextPage()}
          style={{
            marginInlineStart: "var(--space-2)",
            color: "var(--accent)",
            background: "none",
            border: "none",
            fontFamily: "var(--mono-font)",
            cursor: "pointer",
          }}
        >
          {search.isFetchingNextPage
            ? t("table.loading")
            : t("search.loadMore", { defaultValue: "load next server page" })}
        </button>
      )}
    </section>
  );
}

export function ForensicSearch(props: ForensicSearchProps) {
  const { t } = useTranslation();
  const [draft, setDraft] = useState("");
  const [committed, setCommitted] = useState("");
  const [groupStatus, setGroupStatus] = useState<
    Record<string, { state: "pending" | "success" | "error"; matched: number }>
  >({});
  const inputRef = useRef<HTMLInputElement>(null);
  const dialogRef = useRef<HTMLFormElement>(null);
  const restoreFocus = useRef<HTMLElement | null>(null);

  useEffect(() => {
    if (!props.open) return;
    restoreFocus.current =
      document.activeElement instanceof HTMLElement
        ? document.activeElement
        : null;
    inputRef.current?.focus();
    return () => restoreFocus.current?.focus();
  }, [props.open]);

  useEffect(() => {
    const timeout = window.setTimeout(() => setCommitted(draft.trim()), 200);
    return () => window.clearTimeout(timeout);
  }, [draft]);

  const onGroupStatus = useCallback(
    (
      key: string,
      status: { state: "pending" | "success" | "error"; matched: number },
    ) => {
      setGroupStatus((previous) => {
        const current = previous[key];
        if (
          current?.state === status.state &&
          current.matched === status.matched
        ) {
          return previous;
        }
        return { ...previous, [key]: status };
      });
    },
    [],
  );

  if (!props.open) return null;
  const visiblePlan = compileForensicSearch(draft.trim(), props.views);
  const serverPlan = compileForensicSearch(committed, props.views);
  const groups = committed === draft.trim() ? serverPlan.groups : [];
  const groupKeys = groups.map((plan) => `${plan.view.code}:${plan.q}`);
  const statuses = groupKeys.map((key) => groupStatus[key]);
  const noMatches =
    statuses.length > 0 &&
    statuses.every(
      (status) => status?.state === "success" && status.matched === 0,
    );
  const resultButtons = () =>
    Array.from(
      dialogRef.current?.querySelectorAll<HTMLButtonElement>(
        "button[data-search-result]",
      ) ?? [],
    );
  const onKeyDown = (event: React.KeyboardEvent) => {
    if (event.key === "Escape") {
      event.preventDefault();
      event.stopPropagation();
      props.onClose();
      return;
    }
    const buttons = resultButtons();
    if (event.key === "Enter") {
      const active = document.activeElement;
      if (active instanceof HTMLButtonElement && active.dataset.searchResult) {
        event.preventDefault();
        active.click();
      } else if (event.target === inputRef.current && draft.trim() !== "") {
        setCommitted(draft.trim());
      }
      return;
    }
    if (event.key !== "ArrowDown" && event.key !== "ArrowUp") return;
    if (buttons.length === 0) return;
    event.preventDefault();
    const current = buttons.indexOf(
      document.activeElement as HTMLButtonElement,
    );
    const direction = event.key === "ArrowDown" ? 1 : -1;
    const next =
      current < 0
        ? direction > 0
          ? 0
          : buttons.length - 1
        : (current + direction + buttons.length) % buttons.length;
    buttons[next]?.focus();
  };

  return (
    <div
      style={{
        position: "fixed",
        inset: 0,
        zIndex: 30,
        display: "grid",
        placeItems: "start center",
        paddingBlockStart: "8vh",
        background: "color-mix(in srgb, var(--bg) 58%, transparent)",
      }}
    >
      <form
        ref={dialogRef}
        role="dialog"
        aria-modal="true"
        aria-label={t("search.title", { defaultValue: "Forensic search" })}
        onSubmit={(event) => event.preventDefault()}
        style={{
          width: "min(920px, calc(100vw - 32px))",
          maxHeight: "min(760px, 84vh)",
          display: "flex",
          flexDirection: "column",
          color: "var(--fg)",
          background: "var(--bg-overlay)",
          border: "1px solid var(--border-strong)",
          borderRadius: "var(--radius-md)",
          boxShadow: "var(--shadow-pop)",
          overflow: "hidden",
        }}
      >
        <div
          style={{
            display: "flex",
            alignItems: "center",
            gap: "var(--space-2)",
            padding: "var(--space-3)",
            borderBlockEnd: "1px solid var(--border)",
          }}
        >
          <span aria-hidden="true" style={{ color: "var(--accent)" }}>
            ⌕
          </span>
          <input
            ref={inputRef}
            type="search"
            value={draft}
            onChange={(event) => setDraft(event.target.value)}
            onKeyDown={onKeyDown}
            placeholder={t("search.placeholder", {
              defaultValue: "pid:18422 · queryid:… · rel:public.orders",
            })}
            aria-describedby="forensic-search-help"
            style={{
              flex: "1 1 auto",
              minWidth: 0,
              color: "var(--fg)",
              background: "transparent",
              border: "none",
              outline: "none",
              fontFamily: "var(--mono-font)",
              fontSize: "var(--text-lg)",
            }}
          />
          <kbd style={{ color: "var(--fg-dim)" }}>esc</kbd>
        </div>
        <div
          id="forensic-search-help"
          style={{
            padding: "var(--space-2) var(--space-3)",
            color: "var(--fg-dim)",
            fontFamily: "var(--mono-font)",
            fontSize: "var(--text-xs)",
          }}
        >
          pid · queryid · planid · rel · index · wait · event · db · user · app
          · cgroup
          {" · "}
          {t("search.serverScope", {
            defaultValue: "server snapshot; lazy SQL excluded",
          })}
        </div>
        <div style={{ overflowY: "auto", minHeight: "120px" }}>
          {visiblePlan.error !== null && (
            <div
              role="alert"
              style={{
                margin: "var(--space-3)",
                padding: "var(--space-2)",
                color: "var(--sev-warn-fg)",
                background: "var(--sev-warn-bg)",
                border: "1px solid var(--sev-warn)",
                borderRadius: "var(--radius-sm)",
                fontFamily: "var(--ui-font)",
              }}
            >
              {errorText(visiblePlan.error, t)}
            </div>
          )}
          {visiblePlan.error === null && draft.trim() === "" && (
            <div
              style={{
                padding: "var(--space-5)",
                color: "var(--fg-dim)",
                fontFamily: "var(--ui-font)",
                textAlign: "center",
              }}
            >
              {t("search.emptyPrompt", {
                defaultValue: "Search every materialized evidence projection",
              })}
            </div>
          )}
          {groups.map((plan) => (
            <SearchResultGroup
              key={`${plan.view.code}:${plan.q}`}
              groupKey={`${plan.view.code}:${plan.q}`}
              plan={plan}
              at={props.at}
              span={props.span}
              onSelect={props.onSelect}
              onResultKeyDown={onKeyDown}
              onStatus={onGroupStatus}
            />
          ))}
          {noMatches && (
            <div
              role="status"
              style={{
                padding: "var(--space-5)",
                color: "var(--fg-dim)",
                fontFamily: "var(--ui-font)",
                textAlign: "center",
              }}
            >
              {t("search.noMatches", {
                defaultValue: "No server-side matches in this time window",
              })}
            </div>
          )}
        </div>
      </form>
    </div>
  );
}
