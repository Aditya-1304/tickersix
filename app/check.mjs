import { readFile } from "node:fs/promises";

const requiredFiles = ["index.html", "styles.css", "app.js", "package.json"];
const requiredMarkers = [
  ["index.html", "TickerSix"],
  ["styles.css", "@media (max-width: 760px)"],
  ["app.js", "REPLAY - FINALIZED HISTORICAL DEVNET ROUND"],
  ["app.js", "COMPARISON_UNAVAILABLE"],
  ["app.js", "competitive_effects"],
];

for (const file of requiredFiles) await readFile(new URL(`./${file}`, import.meta.url));
for (const [file, marker] of requiredMarkers) {
  const contents = await readFile(new URL(`./${file}`, import.meta.url), "utf8");
  if (!contents.includes(marker)) throw new Error(`${file} is missing required marker: ${marker}`);
}

console.log(`TickerSix app checks passed: ${requiredFiles.length} files, ${requiredMarkers.length} contract markers.`);
