#!/usr/bin/env node

import { access, mkdir } from "node:fs/promises";
import { constants } from "node:fs";
import { spawn } from "node:child_process";
import { fileURLToPath } from "node:url";
import puppeteer from "puppeteer-core";

const WEB_DIR = fileURLToPath(new URL("../", import.meta.url));
const OUT_DIR = fileURLToPath(new URL("../demo/shots/", import.meta.url));
const PORT = Number(process.env.PGK_SHELL_PORT ?? 18444);
const BASE = `http://127.0.0.1:${PORT}`;
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

async function waitForStub(child) {
  const deadline = Date.now() + 15_000;
  let lastError;
  while (Date.now() < deadline) {
    if (child.exitCode !== null) {
      throw new Error(`demo stub exited early with code ${child.exitCode}`);
    }
    try {
      const response = await fetch(`${BASE}/v1/ui/catalog`);
      if (response.ok) return;
      lastError = new Error(`demo stub returned HTTP ${response.status}`);
    } catch (error) {
      lastError = error;
    }
    await new Promise((resolve) => setTimeout(resolve, 100));
  }
  throw new Error(`demo stub did not become ready: ${String(lastError)}`);
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
    const visibleRows = [...matrixBody.querySelectorAll("tbody tr")].filter(
      (row) => {
        const rowRect = row.getBoundingClientRect();
        return (
          rowRect.top >= matrixRect.top &&
          rowRect.bottom <= matrixRect.bottom &&
          rowRect.height > 0
        );
      },
    ).length;
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
        analyticalCenter: rect('[data-shell-region="analytical-center"]'),
        matrix: rect('[data-shell-region="ranked-matrix"]'),
        matrixBody: rect('[data-testid="ranked-matrix-body"]'),
        status: rect('[data-shell-region="status"]'),
      },
      matrix: {
        overflowY: matrixStyle.overflowY,
        clientHeight: matrixBody.clientHeight,
        scrollHeight: matrixBody.scrollHeight,
        visibleRows,
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
  for (const name of ["health", "analyticalCenter", "matrix", "status"]) {
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
  env: { ...process.env, PGK_DEMO_PORT: String(PORT) },
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
  await waitForStub(stub);
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
  await page.goto(`${BASE}/#source=local&view=statements&at=${at}&span=3600`, {
    waitUntil: "networkidle0",
  });
  await page.waitForSelector('table[aria-label="statements"] tbody tr', {
    timeout: 15_000,
  });
  const metrics = await measure(page);
  const keyboard = await verifyKeyboardReach(page);
  assertContract(metrics, keyboard);
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
    `forensic shell PASS\n${JSON.stringify({ ...metrics, keyboard }, null, 2)}`,
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
  stub.kill("SIGTERM");
}
