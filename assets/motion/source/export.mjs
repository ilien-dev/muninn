// Renders every scene in scenes.html to a GIF in assets/motion/, once on
// GitHub's dark ground and once on its light one.
//
//   npm install
//   node export.mjs                 # every scene
//   node export.mjs hero flow       # only these
//
// Each scene is a function of time, so a frame is drawn by setting t and taking
// a screenshot; nothing depends on the machine's speed. The GIF starts at the
// scene's poster frame, so a reader with autoplay off still sees a frame that
// tells the story. Needs ffmpeg on PATH. CHROME_PATH picks a local Chromium
// when Playwright's own browser is not installed.
import { chromium } from "playwright";
import { execFileSync } from "node:child_process";
import { mkdirSync, rmSync, statSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const out = join(here, "..");
const FPS = Number(process.env.FPS || 15);

// scene id in scenes.html → file name in assets/motion/
const SCENES = {
  hero: "hero", change: "change", results: "results", flow: "flow",
  "sc-catalog": "catalog", "sc-deadend": "deadend", "sc-rule": "rule",
  "sc-compact": "compact", "sc-commit": "commit", "sc-fault": "fault",
  local: "local", install: "install", divider: "divider", eye: "eye",
};
const wanted = process.argv.slice(2);
const ids = Object.keys(SCENES).filter(id => !wanted.length || wanted.includes(id) || wanted.includes(SCENES[id]));

const browser = await chromium.launch({ executablePath: process.env.CHROME_PATH || undefined });
const page = await browser.newPage();

for (const id of ids) {
  for (const v of ["dark", "light"]) {
    await page.goto(pathToFileURL(join(here, "scenes.html")).href, { waitUntil: "networkidle" });
    await page.evaluate(() => document.fonts.ready);
    const s = await page.evaluate(([id, v]) => window.muninnMotion.prepare(id, v), [id, v]);
    await page.setViewportSize({ width: s.w, height: s.h });
    await page.waitForTimeout(300); // let the logo images decode

    const dir = join(here, "frames", `${SCENES[id]}-${v}`);
    rmSync(dir, { recursive: true, force: true });
    mkdirSync(dir, { recursive: true });
    const svg = page.locator("#export-stage svg");
    const n = Math.round(s.dur * FPS);
    for (let i = 0; i < n; i++) {
      await page.evaluate(t => window.muninnMotion.frame(t), (s.poster + i / FPS) % s.dur);
      await svg.screenshot({ path: join(dir, `${String(i).padStart(4, "0")}.png`) });
    }

    const gif = join(out, `${SCENES[id]}-${v}.gif`);
    execFileSync("ffmpeg", ["-y", "-loglevel", "error", "-framerate", String(FPS), "-i", join(dir, "%04d.png"),
      "-vf", "split[a][b];[a]palettegen=max_colors=64:stats_mode=diff[p];[b][p]paletteuse=dither=bayer:bayer_scale=4:diff_mode=rectangle",
      "-loop", "0", gif]);
    console.log(`${SCENES[id]}-${v}.gif  ${s.w}×${s.h}  ${n} frames  ${(statSync(gif).size / 1024).toFixed(0)} KB`);
  }
}
await browser.close();
