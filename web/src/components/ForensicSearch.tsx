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
import "./ForensicSearch.css";

export interface ForensicSearchProps {
  open: boolean;
  views: ViewSpec[];
  at: string;
  span: number;
  onClose: () => void;
  onSelect: (view: string, entity: string) => void;
}

interface SearchGroupStatus {
  state: "pending" | "success" | "error";
  matched: number;
  unavailable: boolean;
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
  onStatus: (key: string, status: SearchGroupStatus) => void;
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
  const unavailable =
    search.isError ||
    (search.matched === 0 &&
      (search.quality?.unavailable_revision.length ?? 0) > 0);
  useEffect(() => {
    reportStatus(statusKey, {
      state: status,
      matched: search.matched,
      unavailable,
    });
  }, [unavailable, reportStatus, search.matched, status, statusKey]);
  if (!search.isPending && search.matched === 0 && !unavailable) return null;
  return (
    <section
      data-search-group={props.plan.view.code}
      className="forensic-search__group"
    >
      <header className="forensic-search__group-header">
        <strong>
          {t(`tabs.${props.plan.view.code}`, {
            defaultValue: props.plan.view.code,
          })}
        </strong>
        <span className="forensic-search__group-count">
          {search.isPending
            ? t("search.searching", { defaultValue: "searching…" })
            : t("search.count", {
                shown: search.rows.length,
                matched: search.matched,
                defaultValue: `${search.rows.length} / ${search.matched}`,
              })}
        </span>
        <span className="forensic-search__reason">{props.plan.reason}</span>
      </header>
      {unavailable && (
        <div role="status" className="forensic-search__source-empty">
          {t("search.sourceUnavailable", {
            defaultValue: "No data for this source in the selected period",
          })}
        </div>
      )}
      {search.rows.map((row) => (
        <button
          key={row.entity}
          type="button"
          data-search-result
          onClick={() => props.onSelect(props.plan.view.code, row.entity)}
          className="forensic-search__result"
        >
          <span className="forensic-search__result-main">
            <strong>{row.label}</strong>
            <span>
              {row.cells
                .filter((value) => value !== null)
                .slice(0, 3)
                .map(String)
                .join(" · ")}
            </span>
          </span>
          <span className="forensic-search__result-time">
            {search.snapshotTsUs === null
              ? t("search.selectedPeriod", { defaultValue: "selected period" })
              : formatIntervalTime(Number(search.snapshotTsUs))}
          </span>
          <span className="forensic-search__open-result">
            {t("search.openResult", { defaultValue: "Open" })}
          </span>
        </button>
      ))}
      {search.hasNextPage && (
        <button
          type="button"
          disabled={search.isFetchingNextPage}
          onClick={() => void search.fetchNextPage()}
          className="forensic-search__load-more"
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
    Record<string, SearchGroupStatus>
  >({});
  const inputRef = useRef<HTMLInputElement>(null);
  const dialogRef = useRef<HTMLFormElement>(null);
  const restoreFocus = useRef<HTMLElement | null>(null);
  const keyHandlerRef = useRef<(event: KeyboardEvent) => void>(() => {});

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

  useEffect(() => {
    if (!props.open) return;
    const listener = (event: KeyboardEvent) => {
      if (
        event.target instanceof Node &&
        dialogRef.current?.contains(event.target)
      ) {
        keyHandlerRef.current(event);
      }
    };
    document.addEventListener("keydown", listener);
    return () => document.removeEventListener("keydown", listener);
  }, [props.open]);

  const onGroupStatus = useCallback(
    (key: string, status: SearchGroupStatus) => {
      setGroupStatus((previous) => {
        const current = previous[key];
        if (
          current?.state === status.state &&
          current.matched === status.matched &&
          current.unavailable === status.unavailable
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
      (status) =>
        status?.state === "success" &&
        status.matched === 0 &&
        !status.unavailable,
    );
  const resultButtons = () =>
    Array.from(
      dialogRef.current?.querySelectorAll<HTMLButtonElement>(
        "button[data-search-result]",
      ) ?? [],
    );
  const onKeyDown = (event: KeyboardEvent) => {
    if (event.key === "Escape") {
      event.preventDefault();
      event.stopPropagation();
      props.onClose();
      return;
    }
    if (event.key === "Tab") {
      const focusable = Array.from(
        dialogRef.current?.querySelectorAll<HTMLElement>(
          'input:not([disabled]), button:not([disabled]), [href], [tabindex]:not([tabindex="-1"])',
        ) ?? [],
      );
      const first = focusable[0];
      const last = focusable.at(-1);
      if (first === undefined || last === undefined) return;
      if (event.shiftKey && document.activeElement === first) {
        event.preventDefault();
        last.focus();
      } else if (!event.shiftKey && document.activeElement === last) {
        event.preventDefault();
        first.focus();
      }
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
  keyHandlerRef.current = onKeyDown;

  return (
    <div className="forensic-search">
      <button
        type="button"
        tabIndex={-1}
        data-testid="forensic-search-backdrop"
        aria-label={t("search.dismiss", {
          defaultValue: "Dismiss forensic search backdrop",
        })}
        onClick={props.onClose}
        className="forensic-search__backdrop"
      />
      <form
        ref={dialogRef}
        role="dialog"
        tabIndex={-1}
        aria-modal="true"
        aria-label={t("search.title", { defaultValue: "Forensic search" })}
        onSubmit={(event) => event.preventDefault()}
        className="forensic-search__dialog"
      >
        <div className="forensic-search__input-row">
          <input
            ref={inputRef}
            type="search"
            name="forensic-search"
            autoComplete="off"
            spellCheck={false}
            value={draft}
            onChange={(event) => setDraft(event.target.value)}
            placeholder={t("search.placeholder", {
              defaultValue: "pid:18422 · queryid:… · rel:public.orders",
            })}
            aria-describedby="forensic-search-help"
            aria-label={t("search.label", {
              defaultValue: "Search forensic entities",
            })}
            className="forensic-search__input"
          />
          <button
            type="button"
            aria-label={t("search.close", {
              defaultValue: "Close forensic search",
            })}
            onClick={props.onClose}
            className="forensic-search__close"
          >
            Esc
          </button>
        </div>
        <div id="forensic-search-help" className="forensic-search__help">
          <span>
            pid · queryid · planid · rel · index · wait · event · db · user ·
            app · cgroup
          </span>
          <span className="forensic-search__scope">
            {t("search.serverScope", {
              defaultValue: "selected period · available sources",
            })}
          </span>
        </div>
        <div className="forensic-search__results">
          {visiblePlan.error !== null && (
            <div role="alert" className="forensic-search__error">
              {errorText(visiblePlan.error, t)}
            </div>
          )}
          {visiblePlan.error === null && draft.trim() === "" && (
            <div className="forensic-search__empty">
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
              onStatus={onGroupStatus}
            />
          ))}
          {noMatches && (
            <div role="status" className="forensic-search__empty">
              {t("search.noMatches", {
                defaultValue: "No matches for the selected period",
              })}
            </div>
          )}
        </div>
      </form>
    </div>
  );
}
