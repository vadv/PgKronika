#!/usr/bin/env node
// Screenshot harness for the demo stub: renders the v6 shell against
// `npm run demo:stub` in both themes and saves web/demo/shots/{dark,light}.png.
//
// Requires a system Chromium (puppeteer-core carries no browser):
//   CHROME=/path/to/chrome npm run demo:shot   (default /usr/bin/chromium-browser)

import { access, mkdir } from "node:fs/promises";
import { constants } from "node:fs";
import { fileURLToPath } from "node:url";
import puppeteer from "puppeteer-core";

const BASE = process.env.PGK_DEMO_URL ?? "http://127.0.0.1:18444";
const OUT_DIR = fileURLToPath(new URL("../demo/shots/", import.meta.url));
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
      // Try the next system-browser location.
    }
  }
  throw new Error("No Chromium executable found; set CHROME");
}

await mkdir(OUT_DIR, { recursive: true });

const browser = await puppeteer.launch({
  executablePath: await executableChrome(),
  args: [
    "--no-sandbox",
    "--disable-dev-shm-usage",
    "--force-device-scale-factor=1",
  ],
});

// Pin the cursor (`at`): in LIVE mode the shell derives `at` from Date.now()
// on every render, so queries re-fire each second and networkidle0 never
// settles. Replay mode keeps all query keys stable.
const AT = Date.now() * 1000;

for (const theme of ["dark", "light"]) {
  const page = await browser.newPage();
  await page.setViewport({ width: 1920, height: 1080, deviceScaleFactor: 1 });
  await page.evaluateOnNewDocument((t) => {
    localStorage.setItem("pgk-theme", t);
  }, theme);
  await page.goto(`${BASE}/#source=local&view=statements&at=${AT}`, {
    waitUntil: "networkidle0",
  });
  await page.waitForFunction(
    () =>
      document.querySelectorAll('[data-testid="ranked-matrix-body"] tbody tr')
        .length >= 16,
  );
  const path = `${OUT_DIR}${theme}.png`;
  await page.screenshot({ path, fullPage: false });
  console.log(`saved ${path}`);
  await page.close();
}

await browser.close();
