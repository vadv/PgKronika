#!/usr/bin/env node

import { access, mkdir } from "node:fs/promises";
import { constants } from "node:fs";
import { spawn } from "node:child_process";
import { fileURLToPath } from "node:url";
import puppeteer from "puppeteer-core";

const WEB_DIR = fileURLToPath(new URL("../", import.meta.url));
const OUT_DIR = fileURLToPath(new URL("../demo/shots/", import.meta.url));
const REQUESTED_PORT = Number(process.env.PGK_SHELL_PORT ?? 0);
const VIEWPORT = { width: 1920, height: 1080, deviceScaleFactor: 1 };
const SUCCESS_SHOT = `${OUT_DIR}forensic-shell-1920x1080.png`;
const ACTIVITY_SHOT = `${OUT_DIR}forensic-activity-1920x1080.png`;
const PLANS_SHOT = `${OUT_DIR}forensic-plans-1920x1080.png`;
const FAILURE_SHOT = `${OUT_DIR}forensic-shell-1920x1080-failure.png`;

const chromeCandidates = [
  process.env.CHROME,
  "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
  "/Applications/Chromium.app/Contents/MacOS/Chromium",
  "/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge",
  "/usr/bin/chromium-browser",
  "/usr/bin/chromium",
  "/usr/bin/google-chrome",
].filter(Boolean);

async function executableChrome() {
  for (const candidate of chromeCandidates) {
    try {
      await access(candidate, constants.X_OK);
      return candidate;
    } catch {
      // Try the next documented system-browser location.
    }
  }
  throw new Error(
    `No Chromium executable found. Set CHROME (tried: ${chromeCandidates.join(", ")})`,
  );
}

async function waitForStub(child, output) {
  const deadline = Date.now() + 15_000;
  let lastError;
  while (Date.now() < deadline) {
    if (child.exitCode !== null) {
      throw new Error(
        `demo stub exited before its readiness line with code ${child.exitCode}`,
      );
    }
    const ready = output().match(
      /demo stub: http:\/\/127\.0\.0\.1:(\d+) \(static:/,
    );
    if (ready !== null) {
      const base = `http://127.0.0.1:${ready[1]}`;
      try {
        const response = await fetch(`${base}/v1/ui/catalog`);
        if (response.ok && child.exitCode === null) return base;
        lastError = new Error(`demo stub returned HTTP ${response.status}`);
      } catch (error) {
        lastError = error;
      }
    }
    await new Promise((resolve) => setTimeout(resolve, 100));
  }
  throw new Error(`demo stub did not become ready: ${String(lastError)}`);
}

function exitedWithin(child, timeoutMs) {
  if (child.exitCode !== null) return Promise.resolve(true);
  return new Promise((resolve) => {
    const timer = setTimeout(() => {
      child.off("exit", onExit);
      resolve(false);
    }, timeoutMs);
    const onExit = () => {
      clearTimeout(timer);
      resolve(true);
    };
    child.once("exit", onExit);
  });
}

async function stopChild(child) {
  if (child.exitCode !== null) return;
  child.kill("SIGTERM");
  if (await exitedWithin(child, 2_000)) return;
  child.kill("SIGKILL");
  if (!(await exitedWithin(child, 2_000))) {
    throw new Error(`demo stub process ${child.pid ?? "unknown"} did not exit`);
  }
}

async function measure(page) {
  return page.evaluate(() => {
    const required = (selector) => {
      const element = document.querySelector(selector);
      if (!(element instanceof HTMLElement || element instanceof SVGElement)) {
        throw new Error(`missing required selector ${selector}`);
      }
      return element;
    };
    const rect = (selector) => {
      const box = required(selector).getBoundingClientRect();
      return {
        top: box.top,
        bottom: box.bottom,
        left: box.left,
        right: box.right,
        width: box.width,
        height: box.height,
      };
    };

    const matrixBody = required('[data-testid="ranked-matrix-body"]');
    const matrixRect = matrixBody.getBoundingClientRect();
    const tableHead = required('[data-testid="ranked-matrix-body"] thead');
    const evidenceTop = Math.max(
      matrixRect.top,
      tableHead.getBoundingClientRect().bottom,
    );
    const visibleRowHeights = [
      ...matrixBody.querySelectorAll("tbody tr"),
    ].flatMap((row) => {
      const rowRect = row.getBoundingClientRect();
      return rowRect.top >= evidenceTop &&
        rowRect.bottom <= matrixRect.bottom &&
        rowRect.height > 0
        ? [rowRect.height]
        : [];
    });
    const matrixStyle = getComputedStyle(matrixBody);
    const root = document.documentElement;
    const motionProbe = document.createElement("div");
    motionProbe.style.animation = "pgk-pulse 1.4s ease-in-out infinite";
    motionProbe.style.transition = "background 120ms ease-out";
    document.body.append(motionProbe);
    const motionStyle = getComputedStyle(motionProbe);
    const motion = {
      animationDuration: motionStyle.animationDuration,
      animationIterationCount: motionStyle.animationIterationCount,
      transitionDuration: motionStyle.transitionDuration,
    };
    motionProbe.remove();

    return {
      viewport: {
        width: innerWidth,
        height: innerHeight,
        dpr: devicePixelRatio,
      },
      root: {
        scrollHeight: root.scrollHeight,
        clientHeight: root.clientHeight,
      },
      regions: {
        global: rect('[data-shell-region="global-context"]'),
        navigation: rect('[data-shell-region="primary-navigation"]'),
        health: rect('[data-shell-region="health-line"]'),
        screenContext: rect('[data-shell-region="screen-context"]'),
        analyticalCenter: rect('[data-shell-region="analytical-center"]'),
        matrix: rect('[data-shell-region="ranked-matrix"]'),
        matrixBody: rect('[data-testid="ranked-matrix-body"]'),
        status: rect('[data-shell-region="status"]'),
      },
      matrix: {
        overflowY: matrixStyle.overflowY,
        clientHeight: matrixBody.clientHeight,
        scrollHeight: matrixBody.scrollHeight,
        evidenceTop,
        visibleRows: visibleRowHeights.length,
        visibleRowHeights,
      },
      motion,
      rowCount: matrixBody.querySelectorAll("tbody tr").length,
    };
  });
}

async function diagnoseCurrentLayout(page) {
  return page.evaluate(() => {
    const box = (element) => {
      if (element === null) return null;
      const rect = element.getBoundingClientRect();
      return {
        selector:
          element.getAttribute("data-shell-region") ??
          element.getAttribute("data-testid") ??
          element.tagName.toLowerCase(),
        top: rect.top,
        bottom: rect.bottom,
        height: rect.height,
        clientHeight: element.clientHeight,
        scrollHeight: element.scrollHeight,
        overflowY: getComputedStyle(element).overflowY,
      };
    };
    const table = document.querySelector('table[aria-label="statements"]');
    const tableParent = table?.parentElement ?? null;
    const tableRect = tableParent?.getBoundingClientRect() ?? null;
    const visibleRows =
      tableRect === null
        ? 0
        : [...(table?.querySelectorAll("tbody tr") ?? [])].filter((row) => {
            const rect = row.getBoundingClientRect();
            return rect.top >= tableRect.top && rect.bottom <= innerHeight;
          }).length;
    return {
      viewport: {
        width: innerWidth,
        height: innerHeight,
        dpr: devicePixelRatio,
      },
      root: {
        clientHeight: document.documentElement.clientHeight,
        scrollHeight: document.documentElement.scrollHeight,
      },
      shellRegions: [...document.querySelectorAll("[data-shell-region]")].map(
        box,
      ),
      analyticalContent: box(
        document.querySelector('[data-testid="desktop-forensic-content"]'),
      ),
      currentTableParent: box(tableParent),
      currentVisibleRows: visibleRows,
      rowCount: table?.querySelectorAll("tbody tr").length ?? 0,
    };
  });
}

async function verifyKeyboardReach(page) {
  await page.evaluate(() => {
    document.body.focus();
    if (document.activeElement instanceof HTMLElement) {
      document.activeElement.blur();
    }
  });
  const reached = {
    navigation: false,
    health: false,
    matrix: false,
    status: false,
  };
  for (let step = 0; step < 360; step += 1) {
    await page.keyboard.press("Tab");
    const focus = await page.evaluate(() => {
      const active = document.activeElement;
      if (!(active instanceof HTMLElement || active instanceof SVGElement)) {
        return null;
      }
      return {
        inNavigation:
          active.closest('[data-shell-region="primary-navigation"]') !== null,
        isHealth: active.matches(
          '[data-shell-region="health-line"] [role="slider"]',
        ),
        inMatrix: active.closest('[data-testid="ranked-matrix-body"]') !== null,
        inStatus: active.closest('[data-shell-region="status"]') !== null,
      };
    });
    if (focus === null) continue;
    reached.navigation ||= focus.inNavigation;
    reached.health ||= focus.isHealth;
    reached.matrix ||= focus.inMatrix;
    reached.status ||= focus.inStatus;
    if (Object.values(reached).every(Boolean)) return reached;
  }
  return reached;
}

async function verifyStatementsWorkspace(page) {
  const bodySelector = '[data-testid="ranked-matrix-body"]';
  const loadMoreSelector = '[data-testid="table-load-more"]';
  const read = () =>
    page.evaluate((selector) => {
      const body = document.querySelector(selector);
      if (!(body instanceof HTMLElement)) {
        throw new Error("ranked matrix body is missing");
      }
      const table = body.querySelector('table[aria-label="statements"]');
      const heatmap = document.querySelector(
        '[data-testid="heatmap-time-grid"]',
      );
      const heatmapRequest = performance
        .getEntriesByType("resource")
        .map((entry) => entry.name)
        .find((url) => url.includes("/v1/timeline/heatmap"));
      return {
        loaded: Number(body.dataset.loadedRows ?? "0"),
        rendered: Number(body.dataset.renderedRows ?? "0"),
        domRows: body.querySelectorAll("tr[data-entity]").length,
        spacerRows: body.querySelectorAll("tr[data-virtual-spacer]").length,
        ariaRowCount: Number(table?.getAttribute("aria-rowcount") ?? "0"),
        heatmapCells: heatmap?.querySelectorAll("[data-cell]").length ?? 0,
        heatmapBuckets:
          heatmapRequest === undefined
            ? null
            : Number(new URL(heatmapRequest).searchParams.get("buckets")),
      };
    }, bodySelector);

  const initial = await read();
  const commitFilter = async (value, expectedLoaded, expectedAriaRowCount) => {
    await page.evaluate(async (nextValue) => {
      const search = document.querySelector('input[type="search"]');
      if (!(search instanceof HTMLInputElement)) {
        throw new Error("statement filter is missing");
      }
      const setValue = Object.getOwnPropertyDescriptor(
        HTMLInputElement.prototype,
        "value",
      )?.set;
      if (setValue === undefined) {
        throw new Error("input value setter is missing");
      }
      setValue.call(search, nextValue);
      search.dispatchEvent(new Event("input", { bubbles: true }));
      search.focus();
      await new Promise((resolve) =>
        requestAnimationFrame(() => requestAnimationFrame(resolve)),
      );
    }, value);
    await page.keyboard.press("Enter");
    await page.waitForFunction(
      (selector, loaded, rowCount) => {
        const body = document.querySelector(selector);
        const table = body?.querySelector('table[aria-label="statements"]');
        return (
          body instanceof HTMLElement &&
          Number(body.dataset.loadedRows ?? "-1") === loaded &&
          Number(table?.getAttribute("aria-rowcount") ?? "-1") === rowCount
        );
      },
      { timeout: 10_000 },
      bodySelector,
      expectedLoaded,
      expectedAriaRowCount,
    );
    return read();
  };
  const lazySqlFilter = await commitFilter("*cart_items*", 0, 1);
  const databaseFilter = await commitFilter("database=orders", 200, 501);
  const restoredFilter = await commitFilter("", 200, 1001);
  const filterSemantics = {
    lazySqlMatched: lazySqlFilter.ariaRowCount - 1,
    databaseMatched: databaseFilter.ariaRowCount - 1,
    restoredMatched: restoredFilter.ariaRowCount - 1,
  };
  const bufferHitRange = await page.evaluate(async () => {
    const state = new URLSearchParams(location.hash.slice(1));
    const params = new URLSearchParams({
      at: state.get("at") ?? String(Date.now() * 1000),
      span: `${state.get("span") ?? "3600"}s`,
      preset: "io",
      limit: "200",
    });
    const response = await fetch(`/v1/frame/statements?${params}`);
    if (!response.ok)
      throw new Error(`buffer lens returned ${response.status}`);
    const frame = await response.json();
    const index = frame.columns.findIndex(
      (column) => column.code === "hit_pct",
    );
    if (index < 0) throw new Error("buffer lens omitted hit_pct");
    const values = frame.rows
      .map((row) => row.cells[index])
      .filter((value) => typeof value === "number");
    return {
      count: values.length,
      min: Math.min(...values),
      max: Math.max(...values),
    };
  });
  const pages = [initial.loaded];
  while (await page.$(loadMoreSelector)) {
    const before = pages.at(-1) ?? 0;
    await page.evaluate((selector) => {
      const body = document.querySelector(selector);
      if (!(body instanceof HTMLElement)) {
        throw new Error("ranked matrix body is missing");
      }
      body.scrollTop = body.scrollHeight;
    }, bodySelector);
    await page.waitForFunction(
      (selector) => {
        const button = document.querySelector(selector);
        return button instanceof HTMLButtonElement && !button.disabled;
      },
      { timeout: 5_000 },
      loadMoreSelector,
    );
    await page.$eval(loadMoreSelector, (button) => button.click());
    await page.waitForFunction(
      (selector, prior) => {
        const body = document.querySelector(selector);
        return (
          body instanceof HTMLElement &&
          Number(body.dataset.loadedRows ?? "0") > prior
        );
      },
      { timeout: 10_000 },
      bodySelector,
      before,
    );
    const current = await read();
    pages.push(current.loaded);
  }

  await page.evaluate((selector) => {
    const body = document.querySelector(selector);
    if (body instanceof HTMLElement) body.scrollTop = 0;
  }, bodySelector);
  await page.waitForFunction(
    (selector) => {
      const body = document.querySelector(selector);
      const first = body?.querySelector("tr[data-entity]");
      return (
        body instanceof HTMLElement && body.scrollTop === 0 && first !== null
      );
    },
    { timeout: 5_000 },
    bodySelector,
  );

  const inputLatencyMs = await page.evaluate(async () => {
    const search = document.querySelector('input[type="search"]');
    if (!(search instanceof HTMLInputElement)) {
      throw new Error("statement filter is missing");
    }
    const setValue = Object.getOwnPropertyDescriptor(
      HTMLInputElement.prototype,
      "value",
    )?.set;
    if (setValue === undefined)
      throw new Error("input value setter is missing");
    const update = async (value) => {
      const started = performance.now();
      setValue.call(search, value);
      search.dispatchEvent(new Event("input", { bubbles: true }));
      await new Promise((resolve) =>
        requestAnimationFrame(() => requestAnimationFrame(resolve)),
      );
      return performance.now() - started;
    };
    search.focus();
    const latency = await update("analytics");
    await update("");
    return latency;
  });

  const firstEntity = await page.$eval(
    `${bodySelector} tr[data-entity]`,
    (row) => {
      row.focus();
      return row.getAttribute("data-entity");
    },
  );
  await page.keyboard.press("ArrowDown");
  await page.waitForFunction(
    (entity) =>
      document.activeElement?.getAttribute("data-entity") !== entity &&
      document.activeElement?.hasAttribute("data-entity") === true,
    { timeout: 5_000 },
    firstEntity,
  );
  const arrowEntity = await page.evaluate(() =>
    document.activeElement?.getAttribute("data-entity"),
  );
  await page.keyboard.press("Enter");
  await page.waitForSelector('[data-dock="row"] [data-field="query"]', {
    timeout: 10_000,
  });
  const detail = await page.$eval(
    '[data-dock="row"] [data-field="query"]',
    (field) => ({
      text: field.textContent?.trim() ?? "",
      visible:
        field.getBoundingClientRect().height > 0 ||
        [...field.children].some(
          (child) => child.getBoundingClientRect().height > 0,
        ),
    }),
  );
  await page.keyboard.press("Escape");
  await page.waitForSelector('[data-dock="row"]', {
    hidden: true,
    timeout: 5_000,
  });

  const final = await read();
  const failures = [];
  if (initial.loaded !== 200) {
    failures.push(`initial page loaded ${initial.loaded}, expected 200`);
  }
  if (final.loaded !== 1000) {
    failures.push(`accumulated rows ${final.loaded}, expected 1000`);
  }
  if (final.ariaRowCount !== 1001) {
    failures.push(`aria-rowcount ${final.ariaRowCount}, expected 1001`);
  }
  if (final.rendered > 48 || final.domRows > 48) {
    failures.push(
      `virtual DOM is unbounded: rendered=${final.rendered}, domRows=${final.domRows}`,
    );
  }
  if (final.spacerRows > 2) {
    failures.push(`virtual spacer rows ${final.spacerRows}, expected <= 2`);
  }
  if (initial.heatmapBuckets !== 96 || initial.heatmapCells < 96) {
    failures.push(
      `heatmap contract buckets=${initial.heatmapBuckets}, cells=${initial.heatmapCells}`,
    );
  }
  if (inputLatencyMs > 100) {
    failures.push(
      `filter input latency ${inputLatencyMs.toFixed(1)}ms, expected <= 100ms`,
    );
  }
  if (
    filterSemantics.lazySqlMatched !== 0 ||
    filterSemantics.databaseMatched !== 500 ||
    filterSemantics.restoredMatched !== 1000
  ) {
    failures.push(`filter semantics ${JSON.stringify(filterSemantics)}`);
  }
  if (
    bufferHitRange.count === 0 ||
    bufferHitRange.min < 0 ||
    bufferHitRange.max > 100
  ) {
    failures.push(`invalid buffer hit range ${JSON.stringify(bufferHitRange)}`);
  }
  if (
    firstEntity === null ||
    arrowEntity === null ||
    firstEntity === arrowEntity
  ) {
    failures.push(
      `row keyboard navigation did not advance: ${firstEntity} -> ${arrowEntity}`,
    );
  }
  if (
    !detail.visible ||
    !/(SELECT|UPDATE|INSERT|DELETE)\b/i.test(detail.text)
  ) {
    failures.push(
      `statement detail did not expose bounded SQL: ${detail.text}`,
    );
  }
  if (failures.length > 0) throw new Error(failures.join("\n"));
  return {
    pages,
    final,
    inputLatencyMs,
    filterSemantics,
    bufferHitRange,
    keyboard: { firstEntity, arrowEntity },
    detail,
  };
}

async function verifyGlobalSearchDetail(page) {
  const matrixSelector = '[data-testid="ranked-matrix-body"]';
  const matrixBefore = await page.$eval(matrixSelector, (element) => {
    const rect = element.getBoundingClientRect();
    return {
      left: rect.left,
      top: rect.top,
      width: rect.width,
      height: rect.height,
    };
  });

  await page.keyboard.press("/");
  await page.waitForSelector('[role="dialog"] input[type="search"]', {
    timeout: 5_000,
  });
  const forensicInput = await page.$('[role="dialog"] input[type="search"]');
  if (forensicInput === null)
    throw new Error("forensic search input is missing");
  await page.keyboard.down("Shift");
  await page.keyboard.press("Tab");
  await page.keyboard.up("Shift");
  const trappedBackward = await page.evaluate(
    () =>
      document.activeElement?.getAttribute("aria-label") ===
      "Close forensic search",
  );
  await page.keyboard.press("Tab");
  const trappedForward = await page.evaluate(
    () =>
      document.activeElement?.matches(
        '[role="dialog"] input[type="search"]',
      ) === true,
  );
  await forensicInput.type("queryid:9180220441127101");
  await page.waitForSelector('[role="dialog"] [data-search-result]', {
    timeout: 10_000,
  });
  const searchResultState = await page.evaluate(() => {
    const dialog = document.querySelector('[role="dialog"]');
    const results = dialog?.querySelectorAll("[data-search-result]") ?? [];
    const resources = performance
      .getEntriesByType("resource")
      .map((entry) => entry.name)
      .filter(
        (name) => name.includes("/v1/frame/") && name.includes("queryid"),
      );
    return {
      resultCount: results.length,
      hashHasQuery: new URLSearchParams(location.hash.slice(1)).has("q"),
      resources,
    };
  });
  const searchState = {
    ...searchResultState,
    trappedBackward,
    trappedForward,
  };

  await page.keyboard.press("ArrowDown");
  await page.keyboard.press("Enter");
  await page.waitForSelector('[data-dock="row"] [data-detail-provenance]', {
    timeout: 10_000,
  });
  const pointState = await page.evaluate((selector) => {
    const dock = document.querySelector('[data-dock="row"]');
    const matrix = document.querySelector(selector);
    if (!(dock instanceof HTMLElement) || !(matrix instanceof HTMLElement)) {
      throw new Error("detail dock or ranked matrix is missing");
    }
    const dockRect = dock.getBoundingClientRect();
    const matrixRect = matrix.getBoundingClientRect();
    const params = new URLSearchParams(location.hash.slice(1));
    return {
      dock: {
        left: dockRect.left,
        right: dockRect.right,
        width: dockRect.width,
      },
      matrix: {
        left: matrixRect.left,
        top: matrixRect.top,
        width: matrixRect.width,
        height: matrixRect.height,
      },
      hashView: params.get("view"),
      hashHasQuery: params.has("q"),
      provenance: dock.querySelector("[data-detail-provenance]")?.textContent,
    };
  }, matrixSelector);

  await page.click('[data-detail-tab-trigger="history"]');
  await page.waitForSelector(
    '[data-dock="row"] [data-detail-history] tbody tr',
    {
      timeout: 10_000,
    },
  );
  const initialHistoryQuality = await page.$(
    '[data-dock="row"] [data-history-quality]',
  );
  const initialHistoryQualityVisible = initialHistoryQuality !== null;
  for (const expectedRows of [8, 12]) {
    await page.click('[data-dock="row"] [data-testid="history-load-more"]');
    await page.waitForFunction(
      (rows) =>
        document.querySelectorAll(
          '[data-dock="row"] [data-detail-history] tbody tr',
        ).length === rows,
      { timeout: 10_000 },
      expectedRows,
    );
    if (expectedRows === 8) {
      await page.waitForSelector('[data-dock="row"] [data-history-quality]', {
        timeout: 5_000,
      });
    }
  }
  const historyState = await page.evaluate((initialQualityVisible) => {
    const requests = performance
      .getEntriesByType("resource")
      .map((entry) => entry.name)
      .filter(
        (name) =>
          name.includes("/v1/entity/statements/") &&
          name.includes("from=") &&
          name.includes("columns="),
      );
    const request = requests[0];
    const url = request === undefined ? null : new URL(request);
    return {
      rows: document.querySelectorAll(
        '[data-dock="row"] [data-detail-history] tbody tr',
      ).length,
      request,
      cursors: requests.map((name) => new URL(name).searchParams.get("cursor")),
      quality:
        document.querySelector('[data-dock="row"] [data-history-quality]')
          ?.textContent ?? "",
      qualityGaps: document
        .querySelector('[data-dock="row"] [data-history-quality]')
        ?.getAttribute("data-gaps"),
      qualityGated: document
        .querySelector('[data-dock="row"] [data-history-quality]')
        ?.getAttribute("data-gated"),
      initialQualityVisible,
      hasPointAt: url?.searchParams.has("at") ?? null,
      hasRange:
        url?.searchParams.has("from") === true &&
        url.searchParams.has("to") &&
        url.searchParams.has("columns"),
    };
  }, initialHistoryQualityVisible);

  await page.click('[data-detail-tab-trigger="relationships"]');
  await page.waitForFunction(
    () =>
      document
        .querySelector('[data-dock="row"] [role="tabpanel"]')
        ?.textContent?.includes("statement_plan") === true,
    { timeout: 5_000 },
  );
  const relationship = await page.$eval(
    '[data-dock="row"] [role="tabpanel"]',
    (element) => element.textContent ?? "",
  );

  await page.click('[data-detail-tab-trigger="raw"]');
  await page.waitForSelector('[data-dock="row"] [data-raw-evidence]', {
    timeout: 5_000,
  });
  const rawProjection = await page.$eval(
    '[data-dock="row"] [data-raw-evidence]',
    (element) => element.textContent ?? "",
  );

  const failures = [];
  if (searchState.resultCount < 1)
    failures.push("global search returned no result");
  if (!searchState.trappedBackward || !searchState.trappedForward) {
    failures.push("forensic search did not trap keyboard focus");
  }
  if (searchState.resources.length < 2) {
    failures.push("global search did not fan out through server frame queries");
  }
  if (searchState.hashHasQuery || pointState.hashHasQuery) {
    failures.push("transient forensic search leaked into shareable q state");
  }
  if (pointState.hashView !== "statements") {
    failures.push(`search opened ${pointState.hashView}, expected statements`);
  }
  if (Math.abs(pointState.dock.width - 520) > 0.5) {
    failures.push(
      `desktop detail width ${pointState.dock.width}, expected 520`,
    );
  }
  for (const key of ["left", "top", "width", "height"]) {
    if (Math.abs(pointState.matrix[key] - matrixBefore[key]) > 0.5) {
      failures.push(
        `detail reflowed matrix ${key}: ${matrixBefore[key]} -> ${pointState.matrix[key]}`,
      );
    }
  }
  if (!pointState.provenance?.includes("point projection")) {
    failures.push("summary omitted point projection provenance");
  }
  if (
    historyState.rows !== 12 ||
    historyState.hasRange !== true ||
    historyState.cursors.join(",") !== ",page-2,page-3" ||
    !historyState.quality.includes("partial") ||
    historyState.qualityGaps !== "1" ||
    historyState.qualityGated !== "1" ||
    historyState.initialQualityVisible !== false
  ) {
    failures.push(`history contract failed: ${JSON.stringify(historyState)}`);
  }
  if (historyState.hasPointAt !== false) {
    failures.push("history request incorrectly mixed point at with range mode");
  }
  if (
    !/(best[_ ]effort)/.test(relationship) ||
    !relationship.includes("ossc_queryid_dbid_userid_attribution")
  ) {
    failures.push(`relationship provenance is incomplete: ${relationship}`);
  }
  if (!rawProjection.includes('"mode": "point"')) {
    failures.push("raw tab did not expose the bounded point projection");
  }
  if (failures.length > 0) throw new Error(failures.join("\n"));

  await page.keyboard.press("Escape");
  await page.waitForSelector('[data-dock="row"]', {
    hidden: true,
    timeout: 5_000,
  });
  return {
    searchState,
    pointState,
    historyState,
    relationshipVerified: true,
    rawProjectionVerified: true,
  };
}

async function heatmapBucketsFor(page, view) {
  return page.evaluate((viewCode) => {
    const request = performance
      .getEntriesByType("resource")
      .map((entry) => entry.name)
      .find((name) => {
        if (!name.includes("/v1/timeline/heatmap")) return false;
        return new URL(name).searchParams.get("view") === viewCode;
      });
    return request === undefined
      ? null
      : Number(new URL(request).searchParams.get("buckets"));
  }, view);
}

async function verifyActivityPlansWorkspaces(page, base, at) {
  await page.goto(
    `${base}/#source=local&view=activity&at=${at}&span=3600&preset=waits_locks`,
    { waitUntil: "networkidle0" },
  );
  await page.waitForSelector(
    '[data-testid="workload-evidence-panel"][data-view="activity"]',
    { timeout: 15_000 },
  );
  await page.waitForSelector('[data-testid="activity-lock-lanes"] button', {
    timeout: 10_000,
  });
  const activity = await page.evaluate(() => {
    const center = document.querySelector(
      '[data-testid="workload-analytical-center"]',
    );
    const panel = document.querySelector(
      '[data-testid="workload-evidence-panel"][data-view="activity"]',
    );
    if (!(center instanceof HTMLElement) || !(panel instanceof HTMLElement)) {
      throw new Error("Activity analytical center is incomplete");
    }
    const centerRect = center.getBoundingClientRect();
    const panelRect = panel.getBoundingClientRect();
    const gated = [...document.querySelectorAll('button[aria-disabled="true"]')]
      .map((button) => button.textContent?.trim() ?? "")
      .filter(Boolean);
    return {
      rootHeight: document.documentElement.scrollHeight,
      centerHeight: centerRect.height,
      panelInside:
        panelRect.top >= centerRect.top &&
        panelRect.bottom <= centerRect.bottom,
      pointEvidence:
        document.querySelector('[data-testid="activity-point-evidence"]')
          ?.textContent ?? "",
      lockLanes: document.querySelectorAll(
        '[data-testid="activity-lock-lanes"] button',
      ).length,
      gated,
    };
  });
  activity.heatmapBuckets = await heatmapBucketsFor(page, "activity");
  const activityFailures = [];
  if (activity.rootHeight > 1080)
    activityFailures.push(`root height ${activity.rootHeight}`);
  if (activity.centerHeight !== 156)
    activityFailures.push(`analytical center ${activity.centerHeight}`);
  if (!activity.panelInside) activityFailures.push("panel escapes center");
  if (activity.lockLanes < 1) activityFailures.push("no lock lanes");
  if (!activity.pointEvidence.includes("Short queries"))
    activityFailures.push("point-snapshot sampling caveat is missing");
  if (
    !activity.gated.includes("Memory") ||
    !activity.gated.includes("XID / Horizon")
  )
    activityFailures.push(`gated lenses missing: ${activity.gated.join(", ")}`);
  if (activity.heatmapBuckets !== 96)
    activityFailures.push(`heatmap buckets ${activity.heatmapBuckets}`);
  if (activityFailures.length > 0) {
    throw new Error(`Activity workspace: ${activityFailures.join("; ")}`);
  }
  await page.screenshot({ path: ACTIVITY_SHOT });

  await page.evaluate(() => {
    const search = document.querySelector('input[type="search"]');
    if (!(search instanceof HTMLInputElement)) {
      throw new Error("Activity filter is missing");
    }
    const setValue = Object.getOwnPropertyDescriptor(
      HTMLInputElement.prototype,
      "value",
    )?.set;
    if (setValue === undefined) throw new Error("input setter missing");
    setValue.call(search, "pid=12041");
    search.dispatchEvent(new Event("input", { bubbles: true }));
    search.focus();
  });
  await page.keyboard.press("Enter");
  await page.waitForSelector('table[aria-label="activity"] tr[data-entity]', {
    timeout: 10_000,
  });
  await page.click('table[aria-label="activity"] tr[data-entity]');
  await page.waitForSelector('[data-dock="row"]', { timeout: 10_000 });
  await page.click('[data-detail-tab-trigger="relationships"]');
  await page.waitForFunction(
    () =>
      document
        .querySelector('[data-dock="row"] [role="tabpanel"]')
        ?.textContent?.includes("activity_process") === true,
    { timeout: 5_000 },
  );
  const processRelation = await page.$eval(
    '[data-dock="row"] [role="tabpanel"]',
    (element) => element.textContent ?? "",
  );
  if (
    !processRelation.includes("same_snapshot_unique_pid") ||
    !/best[_ ]effort/.test(processRelation)
  ) {
    throw new Error(`Activity process provenance: ${processRelation}`);
  }

  await page.goto(
    `${base}/#source=local&view=plans&at=${at}&span=3600&preset=change_timeline`,
    { waitUntil: "networkidle0" },
  );
  await page.waitForSelector(
    '[data-testid="workload-evidence-panel"][data-view="plans"]',
    { timeout: 15_000 },
  );
  await page.waitForSelector('[data-testid="plan-version-lanes"] button', {
    timeout: 10_000,
  });
  const plans = await page.evaluate(() => {
    const panel = document.querySelector(
      '[data-testid="workload-evidence-panel"][data-view="plans"]',
    );
    const panelText = panel?.textContent ?? "";
    const compare = [...document.querySelectorAll("button")].find(
      (button) => button.textContent?.trim() === "Compare",
    );
    return {
      rootHeight: document.documentElement.scrollHeight,
      lanes: document.querySelectorAll(
        '[data-testid="plan-version-lanes"] button',
      ).length,
      ossc: panelText.includes("ossc_queryid_dbid_userid_attribution"),
      vadv: panelText.includes(
        "vadv_queryid_stat_statements_dbid_userid_attribution",
      ),
      compareGated: compare?.getAttribute("aria-disabled") === "true",
    };
  });
  plans.heatmapBuckets = await heatmapBucketsFor(page, "plans");
  const plansFailures = [];
  if (plans.rootHeight > 1080)
    plansFailures.push(`root height ${plans.rootHeight}`);
  if (plans.lanes < 1) plansFailures.push("no version lanes");
  if (!plans.ossc || !plans.vadv) plansFailures.push("fork provenance missing");
  if (!plans.compareGated) plansFailures.push("Compare is not gated");
  if (plans.heatmapBuckets !== 96)
    plansFailures.push(`heatmap buckets ${plans.heatmapBuckets}`);
  if (plansFailures.length > 0) {
    throw new Error(`Plans workspace: ${plansFailures.join("; ")}`);
  }
  await page.screenshot({ path: PLANS_SHOT });
  return {
    activity: { ...activity, processRelationVerified: true },
    plans,
  };
}

function assertContract(metrics, keyboard) {
  const failures = [];
  const exactHeight = (name, expected) => {
    const actual = metrics.regions[name].height;
    if (actual !== expected)
      failures.push(`${name} height ${actual}, expected ${expected}`);
  };
  exactHeight("global", 44);
  exactHeight("navigation", 32);
  exactHeight("health", 60);
  exactHeight("status", 24);
  if (
    metrics.regions.screenContext.height < 68 ||
    metrics.regions.screenContext.height > 76
  ) {
    failures.push(
      `screenContext height ${metrics.regions.screenContext.height}, expected 68..76`,
    );
  }
  if (metrics.viewport.width !== 1920 || metrics.viewport.height !== 1080) {
    failures.push(
      `viewport ${metrics.viewport.width}x${metrics.viewport.height}, expected 1920x1080`,
    );
  }
  if (metrics.viewport.dpr !== 1)
    failures.push(`deviceScaleFactor ${metrics.viewport.dpr}, expected 1`);
  if (metrics.root.scrollHeight > 1080) {
    failures.push(
      `root scrollHeight ${metrics.root.scrollHeight}, expected <= 1080`,
    );
  }
  for (const name of [
    "health",
    "screenContext",
    "analyticalCenter",
    "matrix",
    "status",
  ]) {
    const region = metrics.regions[name];
    if (region.top < 0 || region.bottom > 1080 || region.height <= 0) {
      failures.push(`${name} is not fully visible: ${JSON.stringify(region)}`);
    }
  }
  if (!["auto", "scroll"].includes(metrics.matrix.overflowY)) {
    failures.push(
      `matrix overflowY ${metrics.matrix.overflowY}, expected auto or scroll`,
    );
  }
  if (metrics.matrix.scrollHeight <= metrics.matrix.clientHeight) {
    failures.push(
      `matrix is not independently scrollable: ${metrics.matrix.scrollHeight} <= ${metrics.matrix.clientHeight}`,
    );
  }
  if (metrics.matrix.visibleRows < 16) {
    failures.push(
      `visible matrix rows ${metrics.matrix.visibleRows}, expected >= 16`,
    );
  }
  if (metrics.matrix.visibleRowHeights.some((height) => height < 28)) {
    failures.push(
      `visible matrix row below 28px: ${Math.min(...metrics.matrix.visibleRowHeights)}`,
    );
  }
  const durationMs = (value) =>
    Math.max(
      ...value.split(",").map((part) => {
        const normalized = part.trim();
        return normalized.endsWith("ms")
          ? Number(normalized.slice(0, -2))
          : Number(normalized.slice(0, -1)) * 1000;
      }),
    );
  if (
    durationMs(metrics.motion.animationDuration) > 0.001 ||
    durationMs(metrics.motion.transitionDuration) > 0.001 ||
    Number(metrics.motion.animationIterationCount) > 1
  ) {
    failures.push(
      `reduced motion is not enforced: ${JSON.stringify(metrics.motion)}`,
    );
  }
  for (const [name, reached] of Object.entries(keyboard)) {
    if (!reached)
      failures.push(
        `${name} is not reachable through sequential Tab navigation`,
      );
  }
  if (failures.length > 0) {
    throw new Error(
      `${failures.join("\n")}\nmeasurements: ${JSON.stringify(metrics, null, 2)}`,
    );
  }
}

await mkdir(OUT_DIR, { recursive: true });
const stub = spawn(process.execPath, ["scripts/demo-stub.mjs"], {
  cwd: WEB_DIR,
  env: { ...process.env, PGK_DEMO_PORT: String(REQUESTED_PORT) },
  stdio: ["ignore", "pipe", "pipe"],
});
let stubOutput = "";
stub.stdout.on("data", (chunk) => {
  stubOutput += chunk;
});
stub.stderr.on("data", (chunk) => {
  stubOutput += chunk;
});

let browser;
let page;
try {
  const base = await waitForStub(stub, () => stubOutput);
  browser = await puppeteer.launch({
    executablePath: await executableChrome(),
    headless: true,
    args: [
      "--no-sandbox",
      "--disable-dev-shm-usage",
      "--force-device-scale-factor=1",
    ],
  });
  page = await browser.newPage();
  await page.setViewport(VIEWPORT);
  await page.emulateMediaFeatures([
    { name: "prefers-reduced-motion", value: "reduce" },
  ]);
  await page.evaluateOnNewDocument(() => {
    localStorage.setItem("pgk-theme", "dark");
  });
  const at = Date.now() * 1000;
  await page.goto(`${base}/#source=local&view=statements&at=${at}&span=3600`, {
    waitUntil: "networkidle0",
  });
  await page.waitForSelector('table[aria-label="statements"] tbody tr', {
    timeout: 15_000,
  });
  const metrics = await measure(page);
  const keyboard = await verifyKeyboardReach(page);
  assertContract(metrics, keyboard);
  const statements = await verifyStatementsWorkspace(page);
  const globalSearchDetail = await verifyGlobalSearchDetail(page);
  await page.evaluate(() => {
    const matrixBody = document.querySelector(
      '[data-testid="ranked-matrix-body"]',
    );
    if (matrixBody instanceof HTMLElement) matrixBody.scrollTop = 0;
    if (document.activeElement instanceof HTMLElement)
      document.activeElement.blur();
  });
  await page.screenshot({ path: SUCCESS_SHOT });
  const activityPlans = await verifyActivityPlansWorkspaces(page, base, at);
  console.log(
    `forensic shell PASS\n${JSON.stringify(
      { ...metrics, keyboard, statements, globalSearchDetail, activityPlans },
      null,
      2,
    )}`,
  );
  console.log(`approved screenshot: ${SUCCESS_SHOT}`);
  console.log(`approved screenshot: ${ACTIVITY_SHOT}`);
  console.log(`approved screenshot: ${PLANS_SHOT}`);
} catch (error) {
  if (page !== undefined) {
    console.error(
      `current layout measurements:\n${JSON.stringify(await diagnoseCurrentLayout(page), null, 2)}`,
    );
    await page.screenshot({ path: FAILURE_SHOT });
  }
  console.error(`forensic shell FAIL\n${String(error)}`);
  if (stubOutput.trim() !== "")
    console.error(`demo stub output:\n${stubOutput.trim()}`);
  if (page !== undefined)
    console.error(`diagnostic screenshot: ${FAILURE_SHOT}`);
  process.exitCode = 1;
} finally {
  if (browser !== undefined) await browser.close();
  await stopChild(stub);
}
