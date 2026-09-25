import { readFile } from "node:fs/promises";

const requiredFiles = ["index.html", "styles.css", "app.js", "wallet-client.js", "wallet-client.test.mjs", "package.json"];
const requiredMarkers = [
  ["index.html", "TickerSix"],
  ["styles.css", "@media (max-width: 760px)"],
  ["app.js", "REPLAY - FINALIZED HISTORICAL DEVNET ROUND"],
  ["app.js", "COMPARISON_UNAVAILABLE"],
  ["app.js", "competitive_effects"],
  ["app.js", "PRIVATE_MARKET"],
  ["app.js", "PROVIDER_CLAIMS_NOT_CANONICALLY_COMPARABLE"],
  ["app.js", "/v1/private-markets/assets"],
  ["app.js", "/v1/auth/challenge"],
  ["app.js", "/v1/auth/verify"],
  ["app.js", "/v1/profile/me"],
  ["app.js", "/v1/leagues"],
  ["app.js", "/v1/ranked/status"],
  ["app.js", "/v1/market-rounds/"],
  ["app.js", "FROZEN PUBLIC UNIVERSE"],
  ["wallet-client.js", "wallet-standard:register-wallet"],
];

for (const file of requiredFiles) await readFile(new URL(`./${file}`, import.meta.url));
for (const [file, marker] of requiredMarkers) {
  const contents = await readFile(new URL(`./${file}`, import.meta.url), "utf8");
  if (!contents.includes(marker)) throw new Error(`${file} is missing required marker: ${marker}`);
}

console.log(`TickerSix app checks passed: ${requiredFiles.length} files, ${requiredMarkers.length} contract markers.`);
