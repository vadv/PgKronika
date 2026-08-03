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
const ACTIVITY_CPU_SHOT = `${OUT_DIR}forensic-activity-cpu-1920x1080.png`;
const ACTIVITY_WAITS_SHOT = `${OUT_DIR}forensic-activity-waits-1920x1080.png`;
const PROCESS_DETAIL_SHOT = `${OUT_DIR}forensic-process-detail-1920x1080.png`;
const PLANS_SHOT = `${OUT_DIR}forensic-plans-1920x1080.png`;
const OS_SHOT = `${OUT_DIR}forensic-os-1920x1080.png`;
const TABLES_SHOT = `${OUT_DIR}forensic-tables-1920x1080.png`;
const INDEXES_SHOT = `${OUT_DIR}forensic-indexes-1920x1080.png`;
const VACUUM_SHOT = `${OUT_DIR}forensic-vacuum-1920x1080.png`;
const EVENTS_SHOT = `${OUT_DIR}forensic-events-1920x1080.png`;
const STATEMENTS_COMPACT_SHOT = `${OUT_DIR}forensic-statements-1440x900.png`;
const EVENTS_COMPACT_SHOT = `${OUT_DIR}forensic-events-1440x900.png`;
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
    const optionalRect = (selector) => {
      const element = document.querySelector(selector);
      if (!(element instanceof HTMLElement || element instanceof SVGElement))
        return null;
      const box = element.getBoundingClientRect();
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
        scrollWidth: root.scrollWidth,
        clientWidth: root.clientWidth,
        scrollY,
      },
      regions: {
        global: rect('[data-shell-region="global-context"]'),
        navigation: rect('[data-shell-region="primary-navigation"]'),
        health: rect('[data-shell-region="health-line"]'),
        screenContext: rect('[data-shell-region="screen-context"]'),
        analyticalCenter: optionalRect(
          '[data-shell-region="analytical-center"]',
        ),
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
      statements: {
        workspace: rect('[data-testid="statements-workspace"]'),
        controls: rect(".statements-workspace__controls"),
        timeMatrix: rect('[data-testid="statements-time-matrix"]'),
        timeline: rect(".statements-time-matrix__timeline"),
        detachedHeatmap:
          document.querySelector('[data-testid="heatmap-time-grid"]') !== null,
        temporalRows: document.querySelectorAll(
          '[data-testid="statements-time-matrix"] [data-testid="temporal-row"]',
        ).length,
        bucketCells: document.querySelectorAll(
          '[data-testid="statements-time-matrix"] [data-testid="time-matrix-bucket"]',
        ).length,
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
      const heatmapRequest = performance
        .getEntriesByType("resource")
        .map((entry) => entry.name)
        .find(
          (url) =>
            url.includes("/v1/timeline/heatmap") &&
            new URL(url).searchParams.get("view") === "statements",
        );
      const temporalRows = body.querySelectorAll(
        '[data-testid="temporal-row"]',
      );
      const bucketCells = body.querySelectorAll(
        '[data-testid="time-matrix-bucket"]',
      );
      return {
        loaded: Number(body.dataset.loadedRows ?? "0"),
        rendered: Number(body.dataset.renderedRows ?? "0"),
        domRows: body.querySelectorAll("tr[data-entity]").length,
        spacerRows: body.querySelectorAll("tr[data-virtual-spacer]").length,
        ariaRowCount: Number(table?.getAttribute("aria-rowcount") ?? "0"),
        detachedHeatmap:
          document.querySelector('[data-testid="heatmap-time-grid"]') !== null,
        temporalRows: temporalRows.length,
        bucketCells: bucketCells.length,
        timeMatrixBuckets:
          temporalRows.length === 0
            ? 0
            : temporalRows[0].querySelectorAll(
                '[data-testid="time-matrix-bucket"]',
              ).length,
        heatmapBuckets:
          heatmapRequest === undefined
            ? null
            : Number(new URL(heatmapRequest).searchParams.get("buckets")),
        heatmapTop:
          heatmapRequest === undefined
            ? null
            : Number(new URL(heatmapRequest).searchParams.get("top")),
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

  const inputLatency = await page.evaluate(async () => {
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
    // Warm the controlled input once, then use a median: a browser GC pause
    // must not masquerade as slow search, while sustained slow renders still
    // fail the 100 ms interaction contract.
    await update("warmup");
    await update("");
    const samples = [];
    for (const value of ["a", "an", "ana", "analy", "analytics"]) {
      samples.push(await update(value));
    }
    await update("");
    const ordered = [...samples].sort((left, right) => left - right);
    return { samples, medianMs: ordered[Math.floor(ordered.length / 2)] };
  });
  const inputLatencyMs = inputLatency.medianMs;

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
  if (
    initial.detachedHeatmap ||
    initial.heatmapBuckets !== 96 ||
    initial.heatmapTop !== 64 ||
    initial.timeMatrixBuckets !== 96 ||
    initial.temporalRows < 1 ||
    initial.bucketCells !== initial.temporalRows * 96
  ) {
    failures.push(
      `integrated heatmap contract ${JSON.stringify({
        detached: initial.detachedHeatmap,
        buckets: initial.heatmapBuckets,
        top: initial.heatmapTop,
        rowBuckets: initial.timeMatrixBuckets,
        temporalRows: initial.temporalRows,
        cells: initial.bucketCells,
      })}`,
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
    inputLatencySamples: inputLatency.samples,
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
  await page.waitForSelector('[data-dock="row"] [data-forensic-summary]', {
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
        top: dockRect.top,
        right: dockRect.right,
        bottom: dockRect.bottom,
        width: dockRect.width,
        height: dockRect.height,
      },
      matrix: {
        left: matrixRect.left,
        top: matrixRect.top,
        width: matrixRect.width,
        height: matrixRect.height,
      },
      hashView: params.get("view"),
      hashHasQuery: params.has("q"),
      summary: dock.querySelector('[data-detail-tab="summary"]')?.textContent,
      tokenInSummary:
        dock
          .querySelector('[data-detail-tab="summary"]')
          ?.textContent?.includes(params.get("entity") ?? "") ?? false,
      grouped: dock.querySelectorAll("[data-forensic-group]").length,
    };
  }, matrixSelector);

  await page.click('[data-detail-tab-trigger="history"]');
  await page.waitForSelector(
    '[data-dock="row"] [data-detail-history] tbody tr',
    {
      timeout: 10_000,
    },
  );
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
  }
  const historyState = await page.evaluate(() => {
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
      normalChrome:
        document.querySelector('[data-dock="row"] [role="tabpanel"]')
          ?.textContent ?? "",
      qualityBanner:
        document.querySelector('[data-dock="row"] [data-history-quality]') !==
        null,
      hasPointAt: url?.searchParams.has("at") ?? null,
      hasRange:
        url?.searchParams.has("from") === true &&
        url.searchParams.has("to") &&
        url.searchParams.has("columns"),
    };
  });

  await page.click('[data-detail-tab-trigger="relationships"]');
  await page.waitForSelector(
    '[data-dock="row"] [data-detail-tab="relationships"] .entity-detail__relation',
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
  if (
    Math.abs(pointState.dock.width - 1920) > 0.5 ||
    Math.abs(pointState.dock.top - 136) > 0.5 ||
    Math.abs(pointState.dock.bottom - 1056) > 0.5
  ) {
    failures.push(
      `desktop detail workspace ${JSON.stringify(pointState.dock)}`,
    );
  }
  for (const key of ["left", "top", "width", "height"]) {
    if (Math.abs(pointState.matrix[key] - matrixBefore[key]) > 0.5) {
      failures.push(
        `detail reflowed matrix ${key}: ${matrixBefore[key]} -> ${pointState.matrix[key]}`,
      );
    }
  }
  if (
    pointState.grouped < 1 ||
    pointState.tokenInSummary ||
    /point projection|best[_ ]effort|gaps|gated/i.test(pointState.summary ?? "")
  ) {
    failures.push(`summary chrome is noisy: ${JSON.stringify(pointState)}`);
  }
  if (
    historyState.rows !== 12 ||
    historyState.hasRange !== true ||
    historyState.cursors.join(",") !== ",page-2,page-3" ||
    historyState.qualityBanner ||
    /partial|gaps|gated/i.test(historyState.normalChrome)
  ) {
    failures.push(`history contract failed: ${JSON.stringify(historyState)}`);
  }
  if (historyState.hasPointAt !== false) {
    failures.push("history request incorrectly mixed point at with range mode");
  }
  if (
    !/plans/i.test(relationship) ||
    /best[_ ]effort|statement_plan|ossc_queryid|proof|exact/i.test(relationship)
  ) {
    failures.push(`relationship chrome is noisy: ${relationship}`);
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
    `${base}/#source=local&view=activity&at=${at}&span=3600&preset=overview`,
    { waitUntil: "networkidle0" },
  );
  await page.waitForSelector('[data-testid="activity-workspace"]', {
    timeout: 15_000,
  });
  await page.waitForSelector(
    'table[aria-label="activity"] tbody tr[data-entity]',
    { timeout: 10_000 },
  );
  await page.evaluate(() => window.scrollTo(0, 0));
  const activity = await page.evaluate(() => {
    const workspace = document.querySelector(
      '[data-testid="activity-workspace"]',
    );
    const matrix = document.querySelector('table[aria-label="activity"]');
    const snapshot = document.querySelector(
      '[data-testid="activity-snapshot-table"]',
    );
    if (
      !(workspace instanceof HTMLElement) ||
      !(matrix instanceof HTMLElement) ||
      !(snapshot instanceof HTMLElement)
    ) {
      throw new Error("Activity joined snapshot is incomplete");
    }
    const workspaceRect = workspace.getBoundingClientRect();
    const matrixRect = matrix.getBoundingClientRect();
    const gated = [...document.querySelectorAll('button[aria-disabled="true"]')]
      .map((button) => button.textContent?.trim() ?? "")
      .filter(Boolean);
    return {
      rootHeight: document.documentElement.scrollHeight,
      scrollY: window.scrollY,
      detachedCenter:
        document.querySelector('[data-testid="workload-analytical-center"]') !==
        null,
      workspaceInsideViewport:
        workspaceRect.top >= 0 && workspaceRect.bottom <= window.innerHeight,
      matrixInsideWorkspace:
        matrixRect.top >= workspaceRect.top &&
        matrixRect.bottom >= workspaceRect.top,
      visibleRows: matrix.querySelectorAll("tbody tr[data-entity]").length,
      sampleBuckets: snapshot.querySelectorAll(
        '[data-testid="time-matrix-bucket"]',
      ).length,
      pointEvidence:
        document.querySelector('[data-testid="activity-point-evidence"]')
          ?.textContent ?? "",
      processLink:
        document
          .querySelector('[data-testid="activity-process-link"]')
          ?.textContent?.trim() ?? "",
      processCaveat:
        document
          .querySelector('[data-testid="activity-process-link"]')
          ?.getAttribute("title") ?? "",
      activeMetric:
        [...document.querySelectorAll(".activity-workspace__metric")]
          .find((button) => button.getAttribute("aria-pressed") === "true")
          ?.textContent?.trim() ?? "",
      gated,
      relationHeaders: snapshot.querySelectorAll(
        '[data-evidence-group="relation"]',
      ).length,
      osHeaders: snapshot.querySelectorAll('[data-evidence-group="os"]').length,
    };
  });
  activity.heatmapBuckets = await heatmapBucketsFor(page, "activity");
  const activityFailures = [];
  if (activity.rootHeight > 1080)
    activityFailures.push(`root height ${activity.rootHeight}`);
  if (activity.scrollY !== 0)
    activityFailures.push(`shell scroll offset ${activity.scrollY}`);
  if (activity.detachedCenter)
    activityFailures.push("legacy detached analytical center is present");
  if (!activity.workspaceInsideViewport)
    activityFailures.push("workspace escapes the 1920x1080 viewport");
  if (!activity.matrixInsideWorkspace)
    activityFailures.push("row matrix is detached from Activity evidence");
  if (activity.visibleRows < 18)
    activityFailures.push(`only ${activity.visibleRows} rows are visible`);
  if (activity.sampleBuckets !== 0)
    activityFailures.push(`overview unexpectedly rendered heatmap buckets`);
  if (!activity.pointEvidence.includes("Short queries"))
    activityFailures.push("point-snapshot sampling caveat is missing");
  if (
    !/linked process/i.test(activity.processLink) ||
    /best[_ ]effort|exact|proof/i.test(activity.processLink)
  )
    activityFailures.push(`process link is noisy: ${activity.processLink}`);
  if (!/share this PID/i.test(activity.processCaveat))
    activityFailures.push("PID link explanation is missing");
  if (
    !activity.gated.includes("Memory") ||
    !activity.gated.includes("XID / Horizon")
  )
    activityFailures.push(`gated lenses missing: ${activity.gated.join(", ")}`);
  if (activity.heatmapBuckets !== null)
    activityFailures.push("overview requested a temporal heatmap");
  if (activity.relationHeaders < 1 || activity.osHeaders < 1)
    activityFailures.push("joined PG / PID / OS column groups are missing");
  if (activityFailures.length > 0) {
    throw new Error(`Activity workspace: ${activityFailures.join("; ")}`);
  }
  await page.screenshot({ path: ACTIVITY_SHOT });

  await page.evaluate(() => {
    const cpu = [...document.querySelectorAll("button")].find(
      (button) =>
        button.textContent?.trim() === "CPU" &&
        button.closest('[aria-label="lenses"]') !== null,
    );
    if (!(cpu instanceof HTMLButtonElement)) {
      throw new Error("Activity CPU lens is missing");
    }
    cpu.click();
  });
  await page.waitForFunction(
    () =>
      document
        .querySelector('[data-testid="activity-workspace"]')
        ?.getAttribute("data-lens") === "cpu" &&
      [...document.querySelectorAll(".activity-workspace__metric")].some(
        (button) =>
          button.textContent?.trim() === "CPU" &&
          button.getAttribute("aria-pressed") === "true",
      ),
    { timeout: 10_000 },
  );
  await page.waitForSelector(
    '[data-testid="activity-time-matrix"] [data-testid="time-matrix-bucket"]',
    { timeout: 10_000 },
  );
  await page.waitForFunction(
    () =>
      performance
        .getEntriesByType("resource")
        .map((entry) => entry.name)
        .some((name) => {
          if (!name.includes("/v1/timeline/heatmap")) return false;
          const request = new URL(name);
          return (
            request.searchParams.get("view") === "activity" &&
            request.searchParams.get("metric") === "cpu" &&
            request.searchParams.get("buckets") === "96"
          );
        }),
    { timeout: 10_000 },
  );
  await page.screenshot({ path: ACTIVITY_CPU_SHOT });

  await page.evaluate(() => {
    const waits = [...document.querySelectorAll("button")].find((button) =>
      /waits.*locks/i.test(button.textContent?.trim() ?? ""),
    );
    if (!(waits instanceof HTMLButtonElement)) {
      throw new Error("Activity Waits & Locks lens is missing");
    }
    waits.click();
  });
  await page.waitForSelector(
    '[data-testid="activity-workspace"][data-lens="waits_locks"] [data-testid="activity-lock-evidence"] button',
    { timeout: 10_000 },
  );
  const waits = await page.evaluate(() => {
    const strip = document.querySelector(
      '[data-testid="activity-lock-evidence"]',
    );
    if (!(strip instanceof HTMLElement)) {
      throw new Error("Activity lock evidence is missing");
    }
    const buttons = [...strip.querySelectorAll("button")];
    const last = buttons.at(-1)?.getBoundingClientRect();
    const stripRect = strip.getBoundingClientRect();
    const metric = [
      ...document.querySelectorAll(".activity-workspace__metric"),
    ].find((button) => button.getAttribute("aria-pressed") === "true");
    return {
      edges: buttons.length,
      provenance: strip.getAttribute("data-provenance"),
      lastInside:
        last !== undefined &&
        last.top >= stripRect.top &&
        last.bottom <= stripRect.bottom,
      metric: metric?.textContent?.trim() ?? "",
      rootHeight: document.documentElement.scrollHeight,
      scrollY: window.scrollY,
    };
  });
  if (
    waits.edges < 1 ||
    waits.provenance !== null ||
    !waits.lastInside ||
    !/wait/i.test(waits.metric) ||
    waits.rootHeight > 1080 ||
    waits.scrollY !== 0
  ) {
    throw new Error(`Activity Waits & Locks: ${JSON.stringify(waits)}`);
  }
  await page.screenshot({ path: ACTIVITY_WAITS_SHOT });

  await page.goto(
    `${base}/#source=local&view=activity&at=${at}&span=3600&preset=overview`,
    { waitUntil: "networkidle0" },
  );
  await page.waitForSelector(
    'table[aria-label="activity"] [data-testid="activity-process-link-cell"]',
    { timeout: 10_000 },
  );
  await page.evaluate(() => {
    const search = document.querySelector('input[name="view-filter"]');
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
  await page.click(
    'table[aria-label="activity"] [data-testid="activity-process-link-cell"]',
  );
  await page.waitForFunction(
    () => {
      const params = new URLSearchParams(location.hash.slice(1));
      return (
        params.get("view") === "processes" &&
        params.get("entity") === "proc:12041" &&
        params.get("preset") === null
      );
    },
    { timeout: 10_000 },
  );
  const processDetail = await page.$eval(
    '[data-dock="row"]',
    (element) => element.textContent ?? "",
  );
  if (processDetail.includes("saved by this deep link")) {
    throw new Error(
      "Activity process drill-down kept an invalid Activity lens",
    );
  }
  const richProcessDetail = await page.$eval(
    '[data-dock="row"]',
    (dock) => ({
      groups: [...dock.querySelectorAll("[data-forensic-group]")].map(
        (group) => group.getAttribute("data-forensic-group"),
      ),
      fields: [...dock.querySelectorAll("[data-field]")].map((field) =>
        field.getAttribute("data-field"),
      ),
      semantics: [...dock.querySelectorAll("[data-semantic]")].map((field) =>
        field.getAttribute("data-semantic"),
      ),
      text: dock.textContent ?? "",
      rootHeight: document.documentElement.scrollHeight,
      scrollY: window.scrollY,
    }),
  );
  const requiredProcessFields = [
    "cpu_user",
    "cpu_system",
    "run_delay",
    "rss",
    "virtual_memory",
    "logical_read_bytes_per_second",
    "cache_served_read_bytes_per_second",
    "read_bytes_per_second",
    "logical_write_bytes_per_second",
    "write_bytes_per_second",
    "command",
  ];
  if (
    richProcessDetail.groups.join(",") !== "compute,ioCache,context" ||
    requiredProcessFields.some(
      (field) => !richProcessDetail.fields.includes(field),
    ) ||
    !richProcessDetail.semantics.includes("estimate") ||
    !richProcessDetail.semantics.includes("R") ||
    /page-cache hits|proof|confidence|exact match|gaps|gated/i.test(
      richProcessDetail.text,
    ) ||
    richProcessDetail.rootHeight > 1080 ||
    richProcessDetail.scrollY !== 0
  ) {
    throw new Error(
      `rich Process Detail is incomplete: ${JSON.stringify(richProcessDetail)}`,
    );
  }
  await page.screenshot({ path: PROCESS_DETAIL_SHOT });

  await page.goto(`${base}/#source=local&view=plans&at=${at}&span=3600`, {
    waitUntil: "networkidle0",
  });
  await page.waitForSelector('[data-testid="plans-workspace"]', {
    timeout: 15_000,
  });
  await page.waitForSelector('[data-testid="plans-time-matrix"]', {
    timeout: 15_000,
  });
  await page.waitForSelector('[data-testid="plan-observation-envelope"]', {
    timeout: 10_000,
  });
  await page.evaluate(() => window.scrollTo(0, 0));
  const plans = await page.evaluate(() => {
    const workspace = document.querySelector('[data-testid="plans-workspace"]');
    const matrix = document.querySelector('[data-testid="plans-time-matrix"]');
    const matrixBody = document.querySelector(
      '[data-testid="plans-workspace"] [data-testid="ranked-matrix-body"]',
    );
    const changeEvidence = document.querySelector(
      '[data-testid="plan-change-evidence"]',
    );
    const temporalRow = document.querySelector(
      '[data-testid="plans-time-matrix"] [data-testid="plans-interval-row"]',
    );
    const global = document.querySelector(
      '[data-shell-region="global-context"]',
    );
    const health = document.querySelector('[data-shell-region="health-line"]');
    if (
      !(workspace instanceof HTMLElement) ||
      !(matrix instanceof HTMLElement) ||
      !(matrixBody instanceof HTMLElement) ||
      !(changeEvidence instanceof HTMLElement) ||
      !(temporalRow instanceof HTMLElement) ||
      !(global instanceof HTMLElement) ||
      !(health instanceof HTMLElement)
    ) {
      throw new Error("Plans row-coupled workspace is incomplete");
    }
    const workspaceRect = workspace.getBoundingClientRect();
    const matrixRect = matrix.getBoundingClientRect();
    const globalRect = global.getBoundingClientRect();
    const healthRect = health.getBoundingClientRect();
    const records = [
      ...changeEvidence.querySelectorAll(
        '[data-testid="plan-observation-envelope"]',
      ),
    ];
    const lastRecordRect = records.at(-1)?.getBoundingClientRect();
    const attributionText =
      document.querySelector('[data-testid="plans-attribution-provenance"]')
        ?.textContent ?? "";
    const compare = [...document.querySelectorAll("button")].find(
      (button) => button.textContent?.trim() === "Compare",
    );
    return {
      rootHeight: document.documentElement.scrollHeight,
      scrollY: window.scrollY,
      globalHeight: globalRect.height,
      globalTop: globalRect.top,
      healthHeight: healthRect.height,
      healthInsideViewport:
        healthRect.top >= 0 && healthRect.bottom <= window.innerHeight,
      detachedCenter:
        document.querySelector('[data-shell-region="analytical-center"]') !==
        null,
      workspaceInsideViewport:
        workspaceRect.top >= 0 && workspaceRect.bottom <= window.innerHeight,
      matrixInsideWorkspace:
        matrixRect.top >= workspaceRect.top &&
        matrixRect.bottom >= workspaceRect.top,
      visibleRows: matrix.querySelectorAll("tbody tr[data-entity]").length,
      loadedRows: Number(matrixBody.dataset.loadedRows ?? "0"),
      temporalRows: matrix.querySelectorAll(
        '[data-testid="plans-interval-row"]',
      ).length,
      sampleBuckets: temporalRow.querySelectorAll(
        '[data-testid="time-matrix-bucket"]',
      ).length,
      matrixScrollable: matrixBody.scrollHeight > matrixBody.clientHeight,
      records: records.length,
      lastRecordInside:
        lastRecordRect !== undefined &&
        lastRecordRect.bottom <= changeEvidence.getBoundingClientRect().bottom,
      changeText: changeEvidence.textContent ?? "",
      attributionText,
      workspaceText: workspace.textContent ?? "",
      activeLens: workspace.dataset.lens ?? "",
      regressionBoundary:
        document.querySelector('[data-testid="plans-regression-boundary"]')
          ?.textContent ?? "",
      coverage:
        document.querySelector(".plans-workspace__coverage")?.textContent ?? "",
      compareGated: compare?.getAttribute("aria-disabled") === "true",
    };
  });
  plans.heatmapBuckets = await heatmapBucketsFor(page, "plans");
  const plansFailures = [];
  if (plans.rootHeight > 1080)
    plansFailures.push(`root height ${plans.rootHeight}`);
  if (plans.scrollY !== 0)
    plansFailures.push(`shell scroll offset ${plans.scrollY}`);
  if (plans.globalHeight !== 44 || plans.globalTop !== 0)
    plansFailures.push(
      `global region ${plans.globalHeight}px at ${plans.globalTop}px`,
    );
  if (plans.healthHeight !== 60 || !plans.healthInsideViewport)
    plansFailures.push(`Health line ${plans.healthHeight}px outside viewport`);
  if (plans.detachedCenter)
    plansFailures.push("legacy detached analytical center is present");
  if (!plans.workspaceInsideViewport)
    plansFailures.push("workspace escapes the 1920x1080 viewport");
  if (!plans.matrixInsideWorkspace)
    plansFailures.push("plan matrix is detached from the workspace");
  if (plans.visibleRows < 18)
    plansFailures.push(`only ${plans.visibleRows} plan rows are visible`);
  if (plans.loadedRows < 200)
    plansFailures.push(`only ${plans.loadedRows} plan rows are loaded`);
  if (plans.temporalRows < 18)
    plansFailures.push(`only ${plans.temporalRows} temporal rows are rendered`);
  if (plans.sampleBuckets !== 96)
    plansFailures.push(`row temporal buckets ${plans.sampleBuckets}`);
  if (!plans.matrixScrollable)
    plansFailures.push("plan matrix is not independently scrollable");
  if (plans.records < 1 || plans.records > 3)
    plansFailures.push(`bounded change records ${plans.records}`);
  if (!plans.lastRecordInside)
    plansFailures.push("last observed plan record is clipped");
  if (!plans.attributionText.includes("Statements"))
    plansFailures.push("human plan-to-statement link is missing");
  if (
    /best[_ -]?effort|exact match|ossc_queryid|vadv_queryid|\bprovenance\b|\bgaps?\b|\bgated\b/i.test(
      `${plans.changeText} ${plans.attributionText}`,
    )
  )
    plansFailures.push("technical linkage jargon escaped into normal Plans UI");
  if (plans.activeLens !== "regression")
    plansFailures.push(`default lens ${plans.activeLens}`);
  if (!/baseline|before|after/i.test(plans.regressionBoundary))
    plansFailures.push("baseline comparison hint is missing");
  if (!/1[,. ]?000/.test(plans.coverage))
    plansFailures.push(`dense population coverage missing: ${plans.coverage}`);
  if (!plans.compareGated) plansFailures.push("Compare is not gated");
  if (plans.heatmapBuckets !== 96)
    plansFailures.push(`heatmap buckets ${plans.heatmapBuckets}`);
  if (plansFailures.length > 0) {
    throw new Error(`Plans workspace: ${plansFailures.join("; ")}`);
  }
  await page.screenshot({ path: PLANS_SHOT });
  await page.evaluate(() => {
    const changesButton = [...document.querySelectorAll("button")].find(
      (button) => button.textContent?.trim() === "Changes",
    );
    if (!(changesButton instanceof HTMLButtonElement)) {
      throw new Error("Plans Changes lens is missing");
    }
    changesButton.click();
  });
  await page.waitForFunction(
    () =>
      new URLSearchParams(location.hash.slice(1)).get("preset") ===
      "change_timeline",
    { timeout: 10_000 },
  );
  plans.changesLensVerified = true;
  return {
    activity: {
      ...activity,
      cpuMetricVerified: true,
      waits,
      processRelationVerified: true,
      processDetailVerified: true,
    },
    plans,
  };
}

async function infrastructureGeometry(page, view) {
  const geometry = await page.evaluate((expectedView) => {
    const center = document.querySelector(
      '[data-testid="infrastructure-analytical-center"]',
    );
    const panel = document.querySelector(
      `[data-testid="infrastructure-evidence-panel"][data-view="${expectedView}"]`,
    );
    const global = document.querySelector(
      '[data-shell-region="global-context"]',
    );
    const health = document.querySelector('[data-shell-region="health-line"]');
    if (
      !(center instanceof HTMLElement) ||
      !(panel instanceof HTMLElement) ||
      !(global instanceof HTMLElement) ||
      !(health instanceof HTMLElement)
    ) {
      throw new Error(`${expectedView} infrastructure shell is incomplete`);
    }
    const centerRect = center.getBoundingClientRect();
    const panelRect = panel.getBoundingClientRect();
    const lastPanelControl = [...panel.querySelectorAll("button")]
      .at(-1)
      ?.getBoundingClientRect();
    const gated = [...document.querySelectorAll('button[aria-disabled="true"]')]
      .map((button) => button.textContent?.trim() ?? "")
      .filter(Boolean);
    return {
      rootHeight: document.documentElement.scrollHeight,
      scrollY: window.scrollY,
      globalHeight: global.getBoundingClientRect().height,
      healthHeight: health.getBoundingClientRect().height,
      centerHeight: centerRect.height,
      panelInside:
        panelRect.top >= centerRect.top &&
        panelRect.bottom <= centerRect.bottom,
      lastPanelControlInside:
        lastPanelControl === undefined ||
        lastPanelControl.bottom <= panelRect.bottom,
      panelText: panel.textContent ?? "",
      gated,
    };
  }, view);
  geometry.heatmapBuckets = await heatmapBucketsFor(page, view);
  return geometry;
}

function assertInfrastructureGeometry(name, geometry) {
  const failures = [];
  if (geometry.rootHeight > 1080)
    failures.push(`root height ${geometry.rootHeight}`);
  if (geometry.scrollY !== 0)
    failures.push(`shell scroll offset ${geometry.scrollY}`);
  if (geometry.globalHeight !== 44)
    failures.push(`global region ${geometry.globalHeight}px`);
  if (geometry.healthHeight !== 60)
    failures.push(`Health line ${geometry.healthHeight}px`);
  if (geometry.centerHeight !== 156)
    failures.push(`analytical center ${geometry.centerHeight}px`);
  if (!geometry.panelInside) failures.push("panel escapes analytical center");
  if (!geometry.lastPanelControlInside)
    failures.push("last evidence control is clipped");
  if (geometry.heatmapBuckets !== 96)
    failures.push(`heatmap buckets ${geometry.heatmapBuckets}`);
  if (failures.length > 0) {
    throw new Error(`${name} workspace: ${failures.join("; ")}`);
  }
}

async function osWorkspaceGeometry(page) {
  const geometry = await page.evaluate(() => {
    const workspace = document.querySelector('[data-testid="os-workspace"]');
    const host = document.querySelector(
      '[data-testid="host-pressure-evidence"][data-view="processes"]',
    );
    const global = document.querySelector(
      '[data-shell-region="global-context"]',
    );
    const health = document.querySelector('[data-shell-region="health-line"]');
    if (
      !(workspace instanceof HTMLElement) ||
      !(host instanceof HTMLElement) ||
      !(global instanceof HTMLElement) ||
      !(health instanceof HTMLElement)
    ) {
      throw new Error("OS process workspace is incomplete");
    }
    const workspaceRect = workspace.getBoundingClientRect();
    const hostRect = host.getBoundingClientRect();
    const matrixRows = document.querySelectorAll(
      ".processes-time-matrix__timeline-cell",
    ).length;
    const signalLanes = [...document.querySelectorAll(".os-host-signal")];
    return {
      rootHeight: document.documentElement.scrollHeight,
      scrollY: window.scrollY,
      globalHeight: global.getBoundingClientRect().height,
      healthHeight: health.getBoundingClientRect().height,
      hostInside:
        hostRect.top >= workspaceRect.top &&
        hostRect.bottom <= workspaceRect.bottom,
      signalLanes: signalLanes.length,
      signalBuckets: signalLanes.map(
        (lane) => lane.querySelectorAll("meter").length,
      ),
      matrixRows,
      hostText: host.textContent ?? "",
      selectedMetric:
        document
          .querySelector('.os-workspace__metric[aria-pressed="true"]')
          ?.textContent?.trim() ?? "",
    };
  });
  geometry.heatmapBuckets = await heatmapBucketsFor(page, "processes");
  return geometry;
}

function assertOsWorkspaceGeometry(geometry) {
  const failures = [];
  if (geometry.rootHeight > 1080)
    failures.push(`root height ${geometry.rootHeight}`);
  if (geometry.scrollY !== 0)
    failures.push(`shell scroll offset ${geometry.scrollY}`);
  if (geometry.globalHeight !== 44)
    failures.push(`global region ${geometry.globalHeight}px`);
  if (geometry.healthHeight !== 60)
    failures.push(`Health line ${geometry.healthHeight}px`);
  if (!geometry.hostInside) failures.push("host evidence escapes workspace");
  if (geometry.signalLanes !== 2)
    failures.push(`host signal lanes ${geometry.signalLanes}`);
  if (geometry.signalBuckets.some((count) => count !== 24))
    failures.push(`host signal buckets ${geometry.signalBuckets.join(",")}`);
  if (geometry.matrixRows < 10)
    failures.push(`process matrix rows ${geometry.matrixRows}`);
  if (geometry.heatmapBuckets !== 96)
    failures.push(`process heatmap buckets ${geometry.heatmapBuckets}`);
  if (!/CPU/i.test(geometry.selectedMetric))
    failures.push(`selected process metric ${geometry.selectedMetric}`);
  if (failures.length > 0) {
    throw new Error(`OS workspace: ${failures.join("; ")}`);
  }
}

async function verifyInfrastructureWorkspaces(page, base, at) {
  await page.goto(
    `${base}/#source=local&view=processes&at=${at}&span=3600&preset=pressure`,
    { waitUntil: "networkidle0" },
  );
  await page.waitForSelector('[data-testid="os-workspace"]', {
    timeout: 15_000,
  });
  await page.evaluate(() => window.scrollTo(0, 0));
  const os = await osWorkspaceGeometry(page);
  assertOsWorkspaceGeometry(os);
  if (
    !/CPU/i.test(os.hostText) ||
    !/kernel/i.test(os.hostText) ||
    !/load\s*\/\s*CPU/i.test(os.hostText) ||
    /partial|gaps|gated|resource.?limited|independent.scopes/i.test(os.hostText)
  ) {
    throw new Error(`OS compact host context: ${JSON.stringify(os)}`);
  }
  const osQualityVisible = await page.$('[data-testid="host-quality"]');
  if (osQualityVisible !== null) {
    throw new Error("OS collection diagnostics escaped into the normal UI");
  }
  await page.screenshot({ path: OS_SHOT });

  await page.goto(
    `${base}/#source=local&view=tables&at=${at}&span=3600&preset=vacuum_risk`,
    { waitUntil: "networkidle0" },
  );
  await page.waitForSelector('[data-testid="table-vacuum-lanes"] button', {
    timeout: 15_000,
  });
  await page.evaluate(() => window.scrollTo(0, 0));
  const tables = await infrastructureGeometry(page, "tables");
  assertInfrastructureGeometry("Tables", tables);
  if (
    !tables.panelText.includes("Vacuum activity") ||
    /independent|not joined|lifetime|provenance/i.test(tables.panelText) ||
    !tables.gated.includes("Growth") ||
    !tables.gated.includes("Dependencies")
  ) {
    throw new Error(
      `Tables temporal/gating contract: ${JSON.stringify(tables)}`,
    );
  }
  await page.screenshot({ path: TABLES_SHOT });
  await page.click('[data-testid="table-vacuum-lanes"] button');
  await page.waitForSelector('[data-dock="row"]', { timeout: 10_000 });
  await page.click('[data-detail-tab-trigger="relationships"]');
  await page.waitForSelector('[data-dock="row"] [role="tabpanel"] button', {
    timeout: 5_000,
  });
  const vacuumRelation = await page.$eval(
    '[data-dock="row"] [role="tabpanel"]',
    (element) => element.textContent ?? "",
  );
  if (
    !/table|vacuum|relation/i.test(vacuumRelation) ||
    /same_snapshot_database_relation_oid|best_effort|temporal|provenance|proof/i.test(
      vacuumRelation,
    )
  )
    throw new Error(`Vacuum/table human relation: ${vacuumRelation}`);

  await page.goto(
    `${base}/#source=local&view=indexes&at=${at}&span=3600&preset=table_context`,
    { waitUntil: "networkidle0" },
  );
  await page.waitForSelector(
    '[data-testid="infrastructure-evidence-panel"][data-view="indexes"]',
    { timeout: 15_000 },
  );
  await page.evaluate(() => window.scrollTo(0, 0));
  const indexes = await infrastructureGeometry(page, "indexes");
  assertInfrastructureGeometry("Indexes", indexes);
  if (
    !/table context/i.test(indexes.panelText) ||
    /same_snapshot_database_relation_oid|best_effort|temporal|provenance|proof/i.test(
      indexes.panelText,
    ) ||
    !indexes.gated.includes("Growth") ||
    !indexes.gated.includes("Duplication") ||
    !indexes.gated.includes("Invalid / build")
  ) {
    throw new Error(
      `Indexes temporal/gating contract: ${JSON.stringify(indexes)}`,
    );
  }
  await page.screenshot({ path: INDEXES_SHOT });

  await page.goto(
    `${base}/#source=local&view=vacuum&at=${at}&span=3600&preset=progress`,
    { waitUntil: "networkidle0" },
  );
  await page.waitForSelector('[data-testid="vacuum-context-summary"]', {
    timeout: 15_000,
  });
  await page.evaluate(() => window.scrollTo(0, 0));
  const vacuum = await infrastructureGeometry(page, "vacuum");
  assertInfrastructureGeometry("Vacuum", vacuum);
  if (
    /provenance|proof|lifetime|PID reuse|datid|relid/i.test(vacuum.panelText) ||
    !vacuum.gated.includes("Wraparound") ||
    !vacuum.gated.includes("Throughput") ||
    !vacuum.gated.includes("Blockers") ||
    !vacuum.gated.includes("History")
  ) {
    throw new Error(`Vacuum compact context: ${JSON.stringify(vacuum)}`);
  }
  await page.screenshot({ path: VACUUM_SHOT });
  return { os, tables, indexes, vacuum, vacuumRelationVerified: true };
}

async function eventGeometry(page) {
  const geometry = await page.evaluate(() => {
    const workspace = document.querySelector(
      '[data-testid="events-workspace"]',
    );
    const overview = document.querySelector(".events-workspace__overview");
    const body = document.querySelector(".events-workspace__body");
    const panel = document.querySelector('[data-testid="events-signal-panel"]');
    const health = document.querySelector('[data-shell-region="health-line"]');
    if (
      !(workspace instanceof HTMLElement) ||
      !(overview instanceof HTMLElement) ||
      !(body instanceof HTMLElement) ||
      !(panel instanceof HTMLElement) ||
      !(health instanceof HTMLElement)
    ) {
      throw new Error("Events range workspace is incomplete");
    }
    const overviewRect = overview.getBoundingClientRect();
    const bodyRect = body.getBoundingClientRect();
    const panelRect = panel.getBoundingClientRect();
    const lanes = [
      ...panel.querySelectorAll('[data-testid="event-signal-lane"]'),
    ];
    const lastLane = lanes.at(-1)?.getBoundingClientRect();
    const gated = [...document.querySelectorAll('button[aria-disabled="true"]')]
      .map((button) => button.textContent?.trim() ?? "")
      .filter(Boolean);
    return {
      viewport: { width: window.innerWidth, height: window.innerHeight },
      rootHeight: document.documentElement.scrollHeight,
      scrollY: window.scrollY,
      healthHeight: health.getBoundingClientRect().height,
      overviewHeight: overviewRect.height,
      panelInside:
        panelRect.top >= overviewRect.top &&
        panelRect.bottom <= overviewRect.bottom,
      bodyInside:
        bodyRect.top >= workspace.getBoundingClientRect().top &&
        bodyRect.bottom <= workspace.getBoundingClientRect().bottom,
      lastLaneInside:
        lastLane === undefined || lastLane.bottom <= panelRect.bottom,
      lanes: lanes.length,
      rangeRows: document.querySelectorAll('[data-testid="event-range-row"]')
        .length,
      families: document.querySelectorAll(".event-family").length,
      eventDensityBuckets: document.querySelectorAll(
        '[data-testid="spine-event-density"]',
      ).length,
      quality:
        document.querySelector('[data-testid="event-signals-quality"]')
          ?.textContent ?? "",
      workspaceText: workspace.textContent ?? "",
      gated,
    };
  });
  geometry.heatmapBuckets = await heatmapBucketsFor(page, "events");
  return geometry;
}

function assertEventsGeometry(name, geometry) {
  const failures = [];
  if (geometry.rootHeight > geometry.viewport.height)
    failures.push(
      `document height ${geometry.rootHeight} > ${geometry.viewport.height}`,
    );
  if (geometry.scrollY !== 0) failures.push(`scrollY ${geometry.scrollY}`);
  if (geometry.healthHeight !== 60)
    failures.push(`Health line ${geometry.healthHeight}px`);
  if (geometry.overviewHeight !== 176)
    failures.push(`events overview ${geometry.overviewHeight}px`);
  if (!geometry.panelInside) failures.push("Signals escapes events overview");
  if (!geometry.bodyInside) failures.push("Events body escapes workspace");
  if (!geometry.lastLaneInside) failures.push("last Signal lane is clipped");
  if (geometry.lanes < 1 || geometry.lanes > 6)
    failures.push(`Signal lanes ${geometry.lanes}, expected 1..6`);
  if (geometry.heatmapBuckets !== 96)
    failures.push(`heatmap buckets ${geometry.heatmapBuckets}`);
  if (geometry.rangeRows < 1)
    failures.push(`selected-range rows ${geometry.rangeRows}`);
  if (geometry.families < 1)
    failures.push(`event families ${geometry.families}`);
  if (geometry.eventDensityBuckets < 1 || geometry.eventDensityBuckets > 48)
    failures.push(
      `Health event density buckets ${geometry.eventDensityBuckets}, expected 1..48`,
    );
  if (geometry.quality !== "")
    failures.push(`quality diagnostics visible: ${geometry.quality}`);
  if (
    /partial|lower_bound|completeness|retention|known loss|\bgated\b/i.test(
      geometry.workspaceText,
    )
  )
    failures.push("technical collection jargon escaped into Events UI");
  if (!geometry.gated.includes("Config changes"))
    failures.push("Config changes is not visibly gated");
  if (failures.length > 0) {
    throw new Error(`${name}: ${failures.join("; ")}`);
  }
}

async function clickButtonByText(page, label) {
  const clicked = await page.evaluate((text) => {
    const button = [...document.querySelectorAll("button")].find(
      (candidate) => candidate.textContent?.trim() === text,
    );
    if (!(button instanceof HTMLButtonElement)) return false;
    button.click();
    return true;
  }, label);
  if (!clicked) throw new Error(`button not found: ${label}`);
}

async function verifyEventsWorkspace(page, base, at) {
  await page.setViewport(VIEWPORT);
  await page.goto(
    `${base}/#source=local&view=events&at=${at}&span=3600&preset=timeline`,
    { waitUntil: "networkidle0" },
  );
  await page.waitForSelector('[data-testid="event-signal-lane"]', {
    timeout: 15_000,
  });
  await page.evaluate(() => {
    window.scrollTo(0, 0);
    if (document.activeElement instanceof HTMLElement)
      document.activeElement.blur();
  });
  await page.keyboard.press("Tab");
  const skipFocused = await page.evaluate(
    () => document.activeElement?.classList.contains("skip-link") === true,
  );
  await page.keyboard.press("Enter");
  const mainFocused = await page.evaluate(
    () => document.activeElement?.id === "main-content",
  );
  if (!skipFocused || !mainFocused)
    throw new Error(
      `Events skip link contract: ${JSON.stringify({ skipFocused, mainFocused })}`,
    );

  const desktop = await eventGeometry(page);
  assertEventsGeometry("Events 1920x1080", desktop);
  const eventRequests = await page.evaluate(() =>
    performance
      .getEntriesByType("resource")
      .map((entry) => entry.name)
      .filter((name) => name.includes("/v1/timeline/events")),
  );
  const eventLimits = eventRequests.map((request) =>
    new URL(request).searchParams.get("limit"),
  );
  if (!eventLimits.includes("50") || !eventLimits.includes("200")) {
    throw new Error(
      `Events range and Signals requests are not bounded: ${eventLimits.join(",")}`,
    );
  }
  await page.screenshot({ path: EVENTS_SHOT });

  await clickButtonByText(page, "Checkpoints");
  await page.waitForFunction(
    () => location.hash.includes("preset=checkpoints"),
    { timeout: 5_000 },
  );
  const checkpointLanes = await page.$$eval(
    '[data-testid="event-signal-lane"]',
    (lanes) => lanes.length,
  );
  if (checkpointLanes !== 2)
    throw new Error(`checkpoint Signal lanes ${checkpointLanes}, expected 2`);
  const checkpointRows = await page.$$eval(
    '[data-testid="event-range-row"]',
    (rows) => rows.length,
  );
  if (checkpointRows !== 2)
    throw new Error(`checkpoint range rows ${checkpointRows}, expected 2`);

  await page.setViewport({ width: 1440, height: 900, deviceScaleFactor: 1 });
  await page.goto(
    `${base}/#source=local&view=events&at=${at}&span=3600&preset=timeline`,
    { waitUntil: "networkidle0" },
  );
  await page.waitForSelector('[data-testid="event-signal-lane"]', {
    timeout: 15_000,
  });
  const compact = await eventGeometry(page);
  assertEventsGeometry("Events 1440x900", compact);
  await page.screenshot({ path: EVENTS_COMPACT_SHOT });

  await clickButtonByText(page, "Errors");
  await page.waitForSelector('[data-testid="event-signal-lane"]', {
    timeout: 5_000,
  });
  await page.click('[data-testid="event-signal-lane"]');
  await page.waitForFunction(() => location.hash.includes("view=tables"), {
    timeout: 5_000,
  });
  const investigationHash = await page.evaluate(() => location.hash);
  if (
    investigationHash.includes("content-derived-orders-db") ||
    investigationHash.includes("entity=") ||
    investigationHash.includes("dock=")
  ) {
    throw new Error(
      `opaque Event identity leaked into route: ${investigationHash}`,
    );
  }
  return {
    desktop,
    compact,
    eventRequests,
    eventLimits,
    checkpointLanes,
    checkpointRows,
    skipFocused,
    mainFocused,
    investigationHash,
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
  if (
    metrics.root.scrollWidth > metrics.root.clientWidth ||
    metrics.root.scrollY !== 0
  ) {
    failures.push(`document owns overflow: ${JSON.stringify(metrics.root)}`);
  }
  for (const name of ["health", "screenContext", "matrix", "status"]) {
    const region = metrics.regions[name];
    if (region.top < 0 || region.bottom > 1080 || region.height <= 0) {
      failures.push(`${name} is not fully visible: ${JSON.stringify(region)}`);
    }
  }
  if (metrics.regions.analyticalCenter !== null) {
    failures.push("Statements still renders a detached analytical center");
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
  if (metrics.matrix.visibleRows < 28 || metrics.matrix.visibleRows > 32) {
    failures.push(
      `visible matrix rows ${metrics.matrix.visibleRows}, expected 28..32`,
    );
  }
  if (metrics.matrix.visibleRowHeights.some((height) => height < 27)) {
    failures.push(
      `visible matrix row below 27px: ${Math.min(...metrics.matrix.visibleRowHeights)}`,
    );
  }
  if (metrics.statements.detachedHeatmap) {
    failures.push("Statements has a detached heatmap grid");
  }
  if (
    metrics.statements.temporalRows < 18 ||
    metrics.statements.temporalRows > 40 ||
    metrics.statements.bucketCells !== metrics.statements.temporalRows * 96 ||
    metrics.statements.bucketCells > 40 * 96
  ) {
    failures.push(
      `bounded temporal DOM failed: ${JSON.stringify(metrics.statements)}`,
    );
  }
  if (
    metrics.statements.timeline.width / metrics.statements.timeMatrix.width <
    0.45
  ) {
    failures.push(
      `timeline ratio ${metrics.statements.timeline.width / metrics.statements.timeMatrix.width}, expected >= 0.45`,
    );
  }
  if (
    metrics.statements.controls.top < 0 ||
    metrics.statements.controls.bottom > 1080 ||
    metrics.statements.controls.height <= 0
  ) {
    failures.push(
      `matrix controls are not visible: ${JSON.stringify(metrics.statements.controls)}`,
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

async function verifyStatementsCompact(page, base, at) {
  await page.setViewport({ width: 1440, height: 900, deviceScaleFactor: 1 });
  await page.goto(`${base}/#source=local&view=statements&at=${at}&span=3600`, {
    waitUntil: "networkidle0",
  });
  await page.waitForSelector(
    '[data-testid="statements-time-matrix"] tr[data-entity]',
    { timeout: 15_000 },
  );
  const compact = await page.evaluate(() => {
    const required = (selector) => {
      const element = document.querySelector(selector);
      if (!(element instanceof HTMLElement))
        throw new Error(`missing compact selector ${selector}`);
      return element;
    };
    const box = (selector) => {
      const rect = required(selector).getBoundingClientRect();
      return {
        top: rect.top,
        bottom: rect.bottom,
        left: rect.left,
        right: rect.right,
        width: rect.width,
        height: rect.height,
      };
    };
    const root = document.documentElement;
    const body = required('[data-testid="ranked-matrix-body"]');
    return {
      viewport: { width: innerWidth, height: innerHeight },
      root: {
        clientWidth: root.clientWidth,
        scrollWidth: root.scrollWidth,
        clientHeight: root.clientHeight,
        scrollHeight: root.scrollHeight,
        scrollY,
      },
      health: box('[data-shell-region="health-line"]'),
      controls: box(".statements-workspace__controls"),
      matrix: box('[data-testid="statements-time-matrix"]'),
      matrixBody: {
        clientWidth: body.clientWidth,
        scrollWidth: body.scrollWidth,
        overflowX: getComputedStyle(body).overflowX,
      },
      detachedHeatmap:
        document.querySelector('[data-testid="heatmap-time-grid"]') !== null,
      buckets:
        document
          .querySelector('[data-testid="temporal-row"]')
          ?.querySelectorAll('[data-testid="time-matrix-bucket"]').length ?? 0,
    };
  });
  const failures = [];
  if (compact.viewport.width !== 1440 || compact.viewport.height !== 900)
    failures.push(`viewport ${JSON.stringify(compact.viewport)}`);
  if (
    compact.root.scrollHeight > compact.root.clientHeight ||
    compact.root.scrollWidth > compact.root.clientWidth ||
    compact.root.scrollY !== 0
  ) {
    failures.push(`root owns overflow ${JSON.stringify(compact.root)}`);
  }
  if (!["auto", "scroll"].includes(compact.matrixBody.overflowX)) {
    failures.push(
      `matrix cannot own horizontal overflow ${JSON.stringify(compact.matrixBody)}`,
    );
  }
  if (
    compact.health.top < 0 ||
    compact.health.bottom > 900 ||
    compact.controls.top < 0 ||
    compact.controls.bottom > 900 ||
    compact.matrix.top < 0 ||
    compact.matrix.bottom <= compact.matrix.top
  ) {
    failures.push("Health line, controls, or matrix is outside the viewport");
  }
  if (compact.detachedHeatmap || compact.buckets !== 96)
    failures.push(
      `compact heatmap detached=${compact.detachedHeatmap}, buckets=${compact.buckets}`,
    );
  if (failures.length > 0)
    throw new Error(
      `Statements 1440x900: ${failures.join("; ")}\n${JSON.stringify(compact, null, 2)}`,
    );
  await page.screenshot({ path: STATEMENTS_COMPACT_SHOT });
  await page.setViewport(VIEWPORT);
  return compact;
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
const browserDiagnostics = [];
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
  page.on("pageerror", (error) => {
    browserDiagnostics.push(`pageerror: ${error.stack ?? error.message}`);
  });
  page.on("console", (message) => {
    if (message.type() === "error")
      browserDiagnostics.push(`console: ${message.text()}`);
  });
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
  const statementsCompact = await verifyStatementsCompact(page, base, at);
  const activityPlans = await verifyActivityPlansWorkspaces(page, base, at);
  const infrastructure = await verifyInfrastructureWorkspaces(page, base, at);
  const events = await verifyEventsWorkspace(page, base, at);
  console.log(
    `forensic shell PASS\n${JSON.stringify(
      {
        ...metrics,
        keyboard,
        statements,
        statementsCompact,
        globalSearchDetail,
        activityPlans,
        infrastructure,
        events,
      },
      null,
      2,
    )}`,
  );
  console.log(`approved screenshot: ${SUCCESS_SHOT}`);
  console.log(`approved screenshot: ${STATEMENTS_COMPACT_SHOT}`);
  console.log(`approved screenshot: ${ACTIVITY_SHOT}`);
  console.log(`approved screenshot: ${ACTIVITY_CPU_SHOT}`);
  console.log(`approved screenshot: ${ACTIVITY_WAITS_SHOT}`);
  console.log(`approved screenshot: ${PROCESS_DETAIL_SHOT}`);
  console.log(`approved screenshot: ${PLANS_SHOT}`);
  console.log(`approved screenshot: ${OS_SHOT}`);
  console.log(`approved screenshot: ${TABLES_SHOT}`);
  console.log(`approved screenshot: ${INDEXES_SHOT}`);
  console.log(`approved screenshot: ${VACUUM_SHOT}`);
  console.log(`approved screenshot: ${EVENTS_SHOT}`);
  console.log(`approved screenshot: ${EVENTS_COMPACT_SHOT}`);
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
  if (browserDiagnostics.length > 0)
    console.error(`browser diagnostics:\n${browserDiagnostics.join("\n")}`);
  if (page !== undefined)
    console.error(`diagnostic screenshot: ${FAILURE_SHOT}`);
  process.exitCode = 1;
} finally {
  if (browser !== undefined) await browser.close();
  await stopChild(stub);
}
