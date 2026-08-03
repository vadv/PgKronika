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
  await page.evaluate(() => {
    const matrixBody = document.querySelector(
      '[data-testid="ranked-matrix-body"]',
    );
    if (matrixBody instanceof HTMLElement) matrixBody.scrollTop = 0;
    if (document.activeElement instanceof HTMLElement)
      document.activeElement.blur();
  });
  await page.screenshot({ path: SUCCESS_SHOT });
  console.log(
    `forensic shell PASS\n${JSON.stringify({ ...metrics, keyboard, statements }, null, 2)}`,
  );
  console.log(`approved screenshot: ${SUCCESS_SHOT}`);
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
