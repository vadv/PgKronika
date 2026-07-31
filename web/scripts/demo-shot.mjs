#!/usr/bin/env node
// Screenshot harness for the demo stub: renders the v6 shell against
// `npm run demo:stub` in both themes and saves web/demo/shots/{dark,light}.png.
//
// Requires a system Chromium (puppeteer-core carries no browser):
//   CHROME=/path/to/chrome npm run demo:shot   (default /usr/bin/chromium-browser)

import { mkdir } from "node:fs/promises";
import { fileURLToPath } from "node:url";
import puppeteer from "puppeteer-core";

const BASE = process.env.PGK_DEMO_URL ?? "http://127.0.0.1:18444";
const OUT_DIR = fileURLToPath(new URL("../demo/shots/", import.meta.url));
const CHROME = process.env.CHROME ?? "/usr/bin/chromium-browser";

await mkdir(OUT_DIR, { recursive: true });

const browser = await puppeteer.launch({
  executablePath: CHROME,
  args: ["--no-sandbox", "--disable-dev-shm-usage"],
});

for (const theme of ["dark", "light"]) {
  const page = await browser.newPage();
  await page.setViewport({ width: 1600, height: 900 });
  await page.evaluateOnNewDocument((t) => {
    localStorage.setItem("pgk-theme", t);
  }, theme);
  await page.goto(`${BASE}/#source=local&view=statements`, {
    waitUntil: "networkidle0",
  });
  // Let the heatmap query settle after networkidle.
  await new Promise((resolve) => setTimeout(resolve, 1000));
  const path = `${OUT_DIR}${theme}.png`;
  await page.screenshot({ path });
  console.log(`saved ${path}`);
  await page.close();
}

await browser.close();
