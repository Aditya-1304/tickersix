/*
 * TickerSix's dependency-free consumer surface.
 *
 * The UI intentionally keeps network reads and local demo data behind the same
 * view model. That lets the hackathon demo run without a paid RPC, while a
 * deployed backend can provide the real round, proof, replay, and SSE data.
 * This client never signs, sends, settles, rates, or awards achievements.
 */

const API_BASE = window.TICKERSIX_API_BASE || "";
const DEMO_BATTLE = "11111111111111111111111111111111";

const SOURCE_INFO = {
  JUPITER_TOKEN_SPOT_V1: {
    queue: "JUPITER ATTESTED",
    projected: "PROJECTED · LIVE TOKEN MARKET · JUPITER",
    final: "FINAL - ATTESTED SOLANA MARKET SETTLEMENT",
    short: "JUPITER",
  },
  PYTH_PRO_VERIFIED_V1: {
    queue: "PYTH VERIFIED",
    projected: "PROJECTED · LIVE MARKET DATA · PYTH PRO",
    final: "FINAL - PYTH VERIFIED ON SOLANA DEVNET",
    short: "PYTH PRO",
  },
};

const DEMO_ROUND = {
  id: 12,
  chain_pubkey: "7zR2...PublicRound12",
  round_sequence: 12,
  state: "SCHEDULED",
  is_replay: false,
  competition_domain: "PUBLIC_EQUITY",
  settlement_source_kind: "JUPITER_TOKEN_SPOT_V1",
  source_trust_label: "JUPITER ATTESTED",
  network: "SOLANA_DEVNET",
  queue_close_at: 0,
  start_target_at: 0,
  end_target_at: 0,
};

const DEMO_ASSETS = [
  { id: 1, symbol: "NVDA", name: "NVIDIA", provider: "xStocks", status: "ELIGIBLE" },
  { id: 2, symbol: "AAPL", name: "Apple", provider: "xStocks", status: "ELIGIBLE" },
  { id: 3, symbol: "META", name: "Meta Platforms", provider: "xStocks", status: "ELIGIBLE" },
  { id: 4, symbol: "MSFT", name: "Microsoft", provider: "xStocks", status: "ELIGIBLE" },
  { id: 5, symbol: "AMZN", name: "Amazon", provider: "xStocks", status: "ELIGIBLE" },
  { id: 6, symbol: "TSLA", name: "Tesla", provider: "xStocks", status: "ELIGIBLE" },
  { id: 7, symbol: "GOOGL", name: "Alphabet", provider: "xStocks", status: "ELIGIBLE" },
  { id: 8, symbol: "AMD", name: "Advanced Micro Devices", provider: "xStocks", status: "ELIGIBLE" },
  { id: 9, symbol: "NFLX", name: "Netflix", provider: "xStocks", status: "ELIGIBLE" },
];

const DEMO_EVENTS = [
  {
    event_id: 100,
    state: "COMMIT_OPEN",
    result: null,
    settlement_source_kind: "JUPITER_TOKEN_SPOT_V1",
    projection_status: "PROJECTED",
    source_label: "PROJECTED · LIVE TOKEN MARKET · JUPITER",
    player_a_score_q9: 0,
    player_b_score_q9: 0,
    as_of: 100,
    recorded_at: 100,
  },
  {
    event_id: 115,
    state: "REVEAL_OPEN",
    result: null,
    settlement_source_kind: "JUPITER_TOKEN_SPOT_V1",
    projection_status: "PROJECTED",
    source_label: "PROJECTED · LIVE TOKEN MARKET · JUPITER",
    player_a_score_q9: 18_200_000,
    player_b_score_q9: 11_300_000,
    as_of: 115,
    recorded_at: 115,
  },
  {
    event_id: 130,
    state: "SETTLEMENT_PENDING",
    result: null,
    settlement_source_kind: "JUPITER_TOKEN_SPOT_V1",
    projection_status: "PROJECTED",
    source_label: "PROJECTED · LIVE TOKEN MARKET · JUPITER",
    player_a_score_q9: 18_200_000,
    player_b_score_q9: 11_300_000,
    as_of: 130,
    recorded_at: 130,
  },
  {
    event_id: 145,
    state: "FINALIZED",
    result: "PLAYER_A",
    settlement_source_kind: "JUPITER_TOKEN_SPOT_V1",
    projection_status: "FINAL",
    source_label: "FINAL - ATTESTED SOLANA MARKET SETTLEMENT",
    player_a_score_q9: 18_200_000,
    player_b_score_q9: 11_300_000,
    as_of: 145,
    recorded_at: 145,
  },
];

const DEMO_REPLAY = {
  battle_pubkey: DEMO_BATTLE,
  market_round_id: 12,
  competition_domain: "PUBLIC_EQUITY",
  originally_rated: true,
  replay: true,
  replay_label: "REPLAY - FINALIZED HISTORICAL DEVNET ROUND",
  competitive_effects: false,
  rating_updates: false,
  achievement_updates: false,
  settlement_source_kind: "JUPITER_TOKEN_SPOT_V1",
  settlement_label: "FINAL - ATTESTED SOLANA MARKET SETTLEMENT",
  proof_path: `/v1/battles/${DEMO_BATTLE}/proof`,
  events: DEMO_EVENTS,
};

const DEMO_PROOF = {
  source_trust_label: "FINAL - ATTESTED SOLANA MARKET SETTLEMENT",
  reconciled_slot: 248_120_551,
  market_round: {
    round_id: 12,
    market_round_pubkey: "7zR2...PublicRound12",
    source_kind: "JUPITER_TOKEN_SPOT_V1",
    price_policy_version: 1,
    quality_policy_version: 1,
    attestor_set_version: 1,
    state: "FINALIZED",
  },
  battle: {
    battle_id: 77,
    battle_pubkey: DEMO_BATTLE,
    market_round_id: 12,
    market_round_pubkey: "7zR2...PublicRound12",
    side_a_lineup: [1, 2, 3, 4, 5, 6],
    side_a_captain: 1,
    side_a_score_q9: 18_200_000,
    side_b_lineup: [1, 2, 3, 4, 5, 7],
    side_b_captain: 2,
    side_b_score_q9: 11_300_000,
    result: "PLAYER_A",
  },
  round_assets: DEMO_ASSETS.slice(0, 6).map((asset) => ({
    asset_id: asset.id,
    symbol: asset.symbol,
    issuer: "xStocks",
    scoring_mint: `${asset.symbol}-devnet-mint`,
    round_asset_pubkey: `${asset.symbol}-round-asset-pda`,
    representation_id: asset.id,
    provider: asset.provider,
    source_kind: "JUPITER_TOKEN_SPOT_V1",
    start: { finalized_price_q9: 100_000_000_000 },
    end: { finalized_price_q9: asset.id === 1 ? 110_000_000_000 : 101_000_000_000 },
    return_q9: asset.id === 1 ? 100_000_000 : 10_000_000,
  })),
  transaction_signatures: ["commit-a-demo", "reveal-a-demo", "settlement-demo"],
};

const state = {
  view: "home",
  round: DEMO_ROUND,
  assets: DEMO_ASSETS,
  selectedAssets: new Set([1, 2, 3, 4, 5, 6]),
  captain: 1,
  battlePubkey: DEMO_BATTLE,
  replay: DEMO_REPLAY,
  proof: DEMO_PROOF,
  replayIndex: DEMO_REPLAY.events.length - 1,
  replayPlaying: false,
  replayTimer: null,
  wallet: null,
  backendOnline: false,
  toast: null,
  basisOpen: false,
};

const app = document.querySelector("#app");

function sourceInfo(kind) {
  return SOURCE_INFO[kind] || SOURCE_INFO.JUPITER_TOKEN_SPOT_V1;
}

function escapeHtml(value) {
  return String(value ?? "")
    .replaceAll("&", "&amp;")
    .replaceAll("<", "&lt;")
    .replaceAll(">", "&gt;")
    .replaceAll('"', "&quot;")
    .replaceAll("'", "&#039;");
}

function formatScore(value) {
  if (value === null || value === undefined) return "—";
  return `${(Number(value) / 10_000_000).toFixed(2)}%`;
}

function shortValue(value) {
  const text = String(value ?? "—");
  return text.length > 24 ? `${text.slice(0, 12)}…${text.slice(-8)}` : text;
}

function showToast(message) {
  state.toast = message;
  render();
  window.setTimeout(() => {
    if (state.toast === message) {
      state.toast = null;
      render();
    }
  }, 3600);
}

async function api(path, options = {}) {
  const response = await fetch(`${API_BASE}${path}`, {
    credentials: "include",
    headers: { Accept: "application/json", ...(options.headers || {}) },
    ...options,
  });
  if (!response.ok) {
    throw new Error((await response.json().catch(() => null))?.error || `HTTP ${response.status}`);
  }
  return response.status === 204 ? null : response.json();
}

async function hydrateBackend() {
  try {
    const round = await api("/v1/market-rounds/next");
    if (round) state.round = round;
    state.backendOnline = true;
  } catch {
    state.backendOnline = false;
  }
}

async function loadReplay() {
  try {
    const replay = await api(`/v1/battles/${state.battlePubkey}/replay`);
    state.replay = replay;
    state.replayIndex = replay.events.length - 1;
  } catch {
    state.replay = DEMO_REPLAY;
    state.replayIndex = DEMO_REPLAY.events.length - 1;
  }
}

async function loadProof() {
  try {
    state.proof = await api(`/v1/battles/${state.battlePubkey}/proof`);
  } catch {
    state.proof = DEMO_PROOF;
  }
}

function renderTopbar() {
  return `
    <header class="topbar">
      <button class="brand" data-action="home" aria-label="Return to TickerSix home">
        <span class="brand-mark">TS</span>
        <span>TICKERSIX</span>
      </button>
      <div class="hero-actions"><span class="cluster-pill">SOLANA DEVNET</span><button class="button ghost" data-action="connect">CONNECT WALLET</button></div>
    </header>`;
}

function renderHome() {
  const source = sourceInfo(state.round.settlement_source_kind);
  return `
    <section class="hero">
      <p class="eyebrow">Stocklana · Public market battles</p>
      <h1>Pick six.<br />Battle the market.</h1>
      <p class="lede">Compete on market calls, not capital. Every Public Ranked round freezes one transparent scoring instrument per reference asset before the queue opens.</p>
      <div class="hero-actions">
        <button class="button" data-action="queue">PLAY RANKED</button>
        <button class="button secondary" data-action="private">PRIVATE MARKETS</button>
      </div>
    </section>

    <section class="grid two">
      <article class="card mode-card interactive" data-action="queue">
        <div>
          <span class="mode-label">PUBLIC RANKED</span>
          <h2>Build a six-asset lineup.</h2>
          <p class="card-copy">One public-equity rating ladder. Frozen source metadata. Source-specific proof after settlement.</p>
        </div>
        <div class="source-line"><span class="source-pill">${escapeHtml(source.queue)}</span><span class="domain-pill">PUBLIC EQUITY</span></div>
      </article>
      <article class="card mode-card private interactive" data-action="private">
        <div>
          <span class="mode-label">PRIVATE MARKETS</span>
          <h2>Provider context, separate domain.</h2>
          <p class="card-copy">PreStocks and Tessera representations stay visibly separate. Exhibition results never silently enter Public Elo.</p>
        </div>
        <div class="source-line"><span class="status-pill">METADATA ONLY</span><span class="domain-pill">PRIVATE MARKET</span></div>
      </article>
    </section>

    <div class="section-heading"><h2>Your Public Progress</h2><span class="muted">Season 1</span></div>
    <section class="grid three">
      <article class="card stat-card"><span class="stat-label">Rating</span><span class="stat-value">Gold · 1584</span><span class="stat-subvalue">+17 this season</span></article>
      <article class="card stat-card"><span class="stat-label">Global rank</span><span class="stat-value">#42</span><span class="stat-subvalue">PUBLIC EQUITY · SEASON 1</span></article>
      <article class="card stat-card"><span class="stat-label">Placement</span><span class="stat-value">5 / 5</span><span class="stat-subvalue">Leaderboard eligible</span></article>
    </section>

    <div class="section-heading"><h2>Recent Battle</h2><button class="button ghost" data-action="replay">OPEN REPLAY</button></div>
    <article class="card">
      <div class="row"><div><h3>Stocklana Open · Battle 77</h3><span class="muted">Round 12 · finalized on Devnet</span></div><span class="source-pill final">FINAL</span></div>
      <div class="source-line"><span class="source-pill final">${escapeHtml(state.replay.settlement_label)}</span><span class="domain-pill">PUBLIC EQUITY</span></div>
      <div class="row"><span class="row-label">Result</span><strong>YOU WIN · +17 Elo</strong></div>
    </article>`;
}

function renderQueue() {
  const source = sourceInfo(state.round.settlement_source_kind);
  return `
    <section class="hero">
      <p class="eyebrow">Public Ranked</p>
      <h1>Queue for the next round.</h1>
      <p class="lede">Matchmaking is scheduled around one shared market window. There is no instant matchmaking and no hidden source switch.</p>
    </section>
    <article class="card">
      <div class="row"><div><h2>NEXT PUBLIC RANKED ROUND</h2><span class="muted">Round ${escapeHtml(state.round.round_sequence)} · frozen public-equity universe</span></div><span class="status-pill">SCHEDULED</span></div>
      <div class="source-line"><span class="source-pill">${escapeHtml(source.queue)}</span><span class="domain-pill">${escapeHtml(state.round.competition_domain)}</span><span class="cluster-pill">${escapeHtml(state.round.network)}</span></div>
      <div class="grid two">
        <div><div class="row"><span class="row-label">Round</span><strong>12:00–16:00 UTC</strong></div><div class="row"><span class="row-label">Queue closes</span><strong>11:45 UTC</strong></div></div>
        <div><div class="row"><span class="row-label">Lineups lock</span><strong>11:55 UTC</strong></div><div class="row"><span class="row-label">Rating</span><strong>Gold · 1584</strong></div></div>
      </div>
      <div class="alert"><strong>Source transparency:</strong> the provider and settlement source are frozen before queue admission. You cannot choose a different provider after freeze.</div>
      <div class="hero-actions"><button class="button" data-action="start-roster">JOIN QUEUE</button><button class="button secondary" data-action="home">BACK HOME</button></div>
      <p class="muted" style="margin: 16px 0 0; font-size: 0.76rem">Demo mode is read-only. A deployed client must complete wallet auth before calling the queue mutation.</p>
    </article>`;
}

function renderRoster() {
  const selectedCount = state.selectedAssets.size;
  const captain = state.assets.find((asset) => asset.id === state.captain);
  return `
    <section class="hero">
      <p class="eyebrow">Round ${escapeHtml(state.round.round_sequence)} · Roster Builder</p>
      <h1>Choose your six.</h1>
      <p class="lede">The Reference Asset is yours to choose. The provider and exact Scoring Instrument are already frozen by the round.</p>
    </section>
    <article class="card">
      <div class="row"><div><h2>FROZEN PUBLIC UNIVERSE</h2><span class="muted">${selectedCount} / 6 slots selected</span></div><span class="source-pill">${escapeHtml(sourceInfo(state.round.settlement_source_kind).queue)}</span></div>
      <div class="progress" aria-label="Roster selection progress"><span style="width:${Math.min(selectedCount / 6, 1) * 100}%"></span></div>
      <div class="asset-grid">
        ${state.assets
          .map(
            (asset) => `
              <label class="asset-card">
                <input type="checkbox" data-asset-id="${asset.id}" ${state.selectedAssets.has(asset.id) ? "checked" : ""} />
                <span class="asset-symbol">${escapeHtml(asset.symbol)}</span>
                <span class="asset-name">${escapeHtml(asset.name)}</span>
                <span class="asset-meta">${escapeHtml(asset.provider)} · ${escapeHtml(asset.status)}</span>
              </label>`,
          )
          .join("")}
      </div>
      <div class="captain-row"><div><strong>Captain</strong><p class="muted" style="margin: 5px 0 0; font-size: 0.78rem">Captain weight is applied by the canonical scoring formula.</p></div><select id="captain-select" aria-label="Choose captain">${[...state.selectedAssets]
        .map((id) => state.assets.find((asset) => asset.id === id))
        .filter(Boolean)
        .map((asset) => `<option value="${asset.id}" ${asset.id === state.captain ? "selected" : ""}>${escapeHtml(asset.symbol)}</option>`)
        .join("")}</select></div>
      <div class="source-line"><span class="domain-pill">Reference identity: canonical</span><span class="domain-pill">Quality: ${selectedCount === 6 ? "ELIGIBLE" : "INCOMPLETE"}</span><span class="domain-pill">Lock: 11:55 UTC</span></div>
      <div class="hero-actions"><button class="button" data-action="battle" ${selectedCount === 6 ? "" : "disabled"}>CONTINUE TO BATTLE</button><button class="button secondary" data-action="queue">BACK TO QUEUE</button></div>
    </article>`;
}

function currentReplayEvent() {
  return state.replay.events[Math.max(0, Math.min(state.replayIndex, state.replay.events.length - 1))] || DEMO_EVENTS.at(-1);
}

function renderBattle() {
  const event = currentReplayEvent();
  const source = sourceInfo(event.settlement_source_kind || state.round.settlement_source_kind);
  return `
    <section class="hero">
      <p class="eyebrow">Battle 77 · Round ${escapeHtml(state.round.round_sequence)}</p>
      <h1>Projected market battle.</h1>
      <p class="lede">Projected values are informational only. Final chain settlement wins if it differs from the live projection.</p>
    </section>
    <article class="card">
      <div class="source-line"><span class="source-pill">${escapeHtml(event.source_label || source.projected)}</span><span class="domain-pill">PUBLIC EQUITY</span><span class="cluster-pill">SOLANA DEVNET</span></div>
      <div class="scoreboard">
        <div class="score-side"><span class="score-name">ADITYA</span><span class="score-value">${formatScore(event.player_a_score_q9)}</span></div>
        <span class="versus">VS</span>
        <div class="score-side"><span class="score-name">QUANTKID</span><span class="score-value">${formatScore(event.player_b_score_q9)}</span></div>
      </div>
      <div class="row"><span class="row-label">Battle state</span><strong>${escapeHtml(event.state)}</strong></div>
      <div class="row"><span class="row-label">Projection status</span><strong>${escapeHtml(event.projection_status)} · NON-AUTHORITATIVE</strong></div>
      <div class="source-line"><button class="button ghost" data-action="toggle-basis">${state.basisOpen ? "HIDE" : "SHOW"} MARKET-INTEGRITY DRAWER</button></div>
      ${state.basisOpen ? `<div class="alert"><strong>COMPARISON_UNAVAILABLE.</strong> The frozen registry did not declare a comparable underlying representation with fresh observations. No basis value is displayed.</div>` : ""}
      <div class="proof-actions"><button class="button" data-action="result">VIEW FINAL RESULT</button><button class="button secondary" data-action="replay">OPEN REPLAY</button></div>
    </article>
    <div class="section-heading"><h2>Your lineup</h2><span class="muted">Captain: NVDA ×2</span></div>
    <section class="grid three">${state.assets
      .slice(0, 6)
      .map((asset, index) => `<article class="card stat-card"><span class="stat-label">${index === 0 ? "CAPTAIN · 2×" : "PICK"}</span><span class="stat-value">${escapeHtml(asset.symbol)}</span><span class="stat-subvalue">${index === 0 ? "+3.20%" : index === 1 ? "+0.70%" : "−0.40%"}</span></article>`)
      .join("")}</section>`;
}

function renderResult() {
  const source = sourceInfo(state.round.settlement_source_kind);
  return `
    <section class="hero">
      <p class="eyebrow">Battle 77 · Result</p>
      <h1>You win.</h1>
      <p class="lede">The authoritative source-specific settlement is complete. This result is the only one eligible for Public Ranked effects.</p>
    </section>
    <article class="card">
      <div class="source-line"><span class="source-pill final">${escapeHtml(source.final)}</span><span class="cluster-pill">SOLANA DEVNET</span></div>
      <div class="grid three">
        <div class="stat-card"><span class="stat-label">Rating delta</span><span class="stat-value">+17 Elo</span><span class="stat-subvalue">1584 → 1601</span></div>
        <div class="stat-card"><span class="stat-label">Global rank</span><span class="stat-value">#42 → #37</span><span class="stat-subvalue">PUBLIC EQUITY · SEASON 1</span></div>
        <div class="stat-card"><span class="stat-label">Settlement</span><span class="stat-value">FINAL</span><span class="stat-subvalue">Round 12</span></div>
      </div>
      <div class="proof-actions"><button class="button" data-action="proof">VIEW PROOF</button><button class="button secondary" data-action="replay">REPLAY ROUND</button><button class="button ghost" data-action="queue">NEXT ROUND</button></div>
    </article>`;
}

function renderProof() {
  const proof = state.proof;
  const battle = proof.battle || DEMO_PROOF.battle;
  const round = proof.market_round || DEMO_PROOF.market_round;
  return `
    <section class="hero">
      <p class="eyebrow">Source-specific proof</p>
      <h1>Every fact has a trail.</h1>
      <p class="lede">Competitive proof is public and replayable. Secrets, commitment preimages, and provider credentials never enter this view.</p>
    </section>
    <article class="card">
      <div class="source-line"><span class="source-pill final">${escapeHtml(proof.source_trust_label)}</span><span class="domain-pill">PUBLIC EQUITY</span><span class="cluster-pill">SOLANA DEVNET</span></div>
      <div class="proof-grid">
        <div class="proof-item"><small>Battle pubkey</small><code>${escapeHtml(shortValue(battle.battle_pubkey))}</code></div>
        <div class="proof-item"><small>Market Round</small><code>${escapeHtml(shortValue(round.market_round_pubkey))}</code></div>
        <div class="proof-item"><small>Source policy</small><code>${escapeHtml(round.source_kind)} · P${round.price_policy_version}</code></div>
        <div class="proof-item"><small>Reconciled slot</small><code>${escapeHtml(proof.reconciled_slot || "CHAIN-RECONCILED")}</code></div>
        <div class="proof-item"><small>Side A wallet</small><code>WalletA · commit/reveal retained</code></div>
        <div class="proof-item"><small>Side B wallet</small><code>WalletB · commit/reveal retained</code></div>
      </div>
      <div class="section-heading"><h2>Canonical lineup</h2><span class="muted">6 picks · captain ${escapeHtml(battle.side_a_captain || 1)}</span></div>
      <div class="grid two"><div class="alert"><strong>Side A</strong><br />${battle.side_a_lineup.join(" · ")}<br />Q9 score: ${escapeHtml(battle.side_a_score_q9)}</div><div class="alert"><strong>Side B</strong><br />${battle.side_b_lineup.join(" · ")}<br />Q9 score: ${escapeHtml(battle.side_b_score_q9)}</div></div>
      <div class="section-heading"><h2>Transactions</h2></div>
      <div class="proof-grid">${(proof.transaction_signatures || []).map((signature) => `<div class="proof-item"><small>Retained signature</small><code>${escapeHtml(signature)}</code></div>`).join("")}</div>
      <div class="proof-actions"><button class="button" data-action="replay">REPLAY FINALIZED ROUND</button><button class="button secondary" data-action="result">BACK TO RESULT</button></div>
    </article>`;
}

function renderReplay() {
  const current = currentReplayEvent();
  const progress = state.replay.events.length ? ((state.replayIndex + 1) / state.replay.events.length) * 100 : 0;
  return `
    <section class="hero">
      <p class="eyebrow">Historical replay</p>
      <h1>Replay the round.</h1>
      <p class="lede">This is a retained finalized timeline. It cannot update Elo, unlock competitive achievements, or refetch another market source.</p>
    </section>
    <article class="card">
      <div class="source-line"><span class="source-pill replay">${escapeHtml(state.replay.replay_label)}</span><span class="domain-pill">${escapeHtml(state.replay.competition_domain)}</span><span class="cluster-pill">SOLANA DEVNET</span></div>
      <div class="alert"><strong>READ-ONLY REPLAY.</strong> Rating updates: disabled. Achievement updates: disabled. Source metadata and proof path: retained.</div>
      <div class="row"><span class="row-label">Current replay state</span><strong>${escapeHtml(current.state)} · ${escapeHtml(current.projection_status)}</strong></div>
      <div class="row"><span class="row-label">Source label</span><strong>${escapeHtml(current.source_label || "SOURCE LABEL UNAVAILABLE")}</strong></div>
      <div class="scoreboard">
        <div class="score-side"><span class="score-name">ADITYA</span><span class="score-value">${formatScore(current.player_a_score_q9)}</span></div>
        <span class="versus">VS</span>
        <div class="score-side"><span class="score-name">QUANTKID</span><span class="score-value">${formatScore(current.player_b_score_q9)}</span></div>
      </div>
      <div class="progress"><span style="width:${progress}%"></span></div>
      <div class="timeline">${state.replay.events
        .map(
          (event, index) => `<div class="timeline-event ${event.projection_status === "FINAL" ? "final" : ""} ${index === state.replayIndex ? "active" : ""}"><span class="timeline-dot"></span><div><div class="timeline-meta"><span>EVENT ${index + 1}</span><span>${escapeHtml(event.as_of)}s</span></div><div class="timeline-title">${escapeHtml(event.state)}${event.result ? ` · ${escapeHtml(event.result)}` : ""}</div><div class="timeline-source">${escapeHtml(event.source_label || "SOURCE LABEL UNAVAILABLE")}</div></div></div>`,
        )
        .join("")}</div>
      <div class="timeline-controls"><button class="button ghost" data-action="replay-toggle">${state.replayPlaying ? "PAUSE" : "PLAY"}</button><input id="replay-range" type="range" min="0" max="${Math.max(0, state.replay.events.length - 1)}" value="${state.replayIndex}" aria-label="Replay position" /><span class="muted">${state.replayIndex + 1}/${state.replay.events.length}</span></div>
      <div class="proof-actions"><button class="button" data-action="proof">VIEW RETAINED PROOF</button><button class="button secondary" data-action="result">FINAL RESULT</button></div>
    </article>`;
}

function renderPrivate() {
  return `
    <section class="hero">
      <p class="eyebrow">Separate competition domain</p>
      <h1>Private Markets.</h1>
      <p class="lede">Provider context is useful. It is not permission to merge different economic claims into Public Equity Elo.</p>
    </section>
    <article class="card">
      <div class="source-line"><span class="domain-pill">PRIVATE MARKET</span><span class="status-pill">METADATA ONLY</span></div>
      <div class="alert"><strong>Exhibition boundary:</strong> PreStocks and Tessera representations can have different legal structures, rights, multipliers, and lifecycle states. Numeric comparison stays unavailable unless a versioned policy permits it.</div>
      <div class="grid two"><div><h3>PreStocks</h3><p class="card-copy">Provider-described private exposure. Not Public Ranked eligible in this build.</p></div><div><h3>Tessera</h3><p class="card-copy">Provider-described participation representation. Not silently treated as equity.</p></div></div>
      <div class="proof-actions"><button class="button secondary" data-action="home">BACK TO PUBLIC RANKED</button></div>
    </article>`;
}

function renderView() {
  switch (state.view) {
    case "queue":
      return renderQueue();
    case "roster":
      return renderRoster();
    case "battle":
      return renderBattle();
    case "result":
      return renderResult();
    case "proof":
      return renderProof();
    case "replay":
      return renderReplay();
    case "private":
      return renderPrivate();
    default:
      return renderHome();
  }
}

function renderNav() {
  const items = [
    ["home", "HOME"],
    ["queue", "RANKED"],
    ["replay", "REPLAY"],
    ["proof", "PROOF"],
  ];
  return `<nav class="bottom-nav" aria-label="Primary navigation">${items
    .map(([view, label]) => `<button class="nav-button ${state.view === view ? "active" : ""}" data-action="${view}">${label}</button>`)
    .join("")}</nav>`;
}

function render() {
  app.innerHTML = `<div class="shell">${renderTopbar()}${renderView()}${renderNav()}</div>${state.toast ? `<div class="toast" role="status">${escapeHtml(state.toast)}</div>` : ""}`;
}

function setView(view) {
  if (state.replayPlaying && view !== "replay") stopReplay();
  state.view = view;
  render();
  window.scrollTo({ top: 0, behavior: "smooth" });
}

function stopReplay() {
  state.replayPlaying = false;
  if (state.replayTimer) window.clearInterval(state.replayTimer);
  state.replayTimer = null;
}

function startReplay() {
  stopReplay();
  state.replayPlaying = true;
  state.replayTimer = window.setInterval(() => {
    if (state.replayIndex >= state.replay.events.length - 1) {
      stopReplay();
      render();
      return;
    }
    state.replayIndex += 1;
    render();
  }, 1200);
  render();
}

async function connectWallet() {
  const provider = window.solana;
  if (!provider?.connect) {
    showToast("No wallet provider detected. Use the read-only demo flow; no signing is required for this UI slice.");
    return;
  }
  try {
    const response = await provider.connect();
    state.wallet = response.publicKey?.toString() || "CONNECTED WALLET";
    showToast(`Wallet connected: ${shortValue(state.wallet)}`);
  } catch {
    showToast("Wallet connection was cancelled. No transaction was created.");
  }
}

async function joinQueue() {
  if (!state.wallet) {
    showToast("This static client is in read-only demo mode. Connect a wallet through the production auth flow before queueing.");
    setView("roster");
    return;
  }
  try {
    await api("/v1/ranked/queue", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ market_round_id: state.round.id }),
    });
    showToast("Queue admission accepted. Continue with the backend-provided Battle.");
    setView("roster");
  } catch (error) {
    showToast(`Queue admission needs backend auth: ${error.message}`);
  }
}

document.addEventListener("click", async (event) => {
  const target = event.target.closest("[data-action]");
  if (!target) return;
  const action = target.dataset.action;
  if (action === "home" || action === "queue" || action === "private" || action === "battle" || action === "result" || action === "proof") {
    if (action === "proof") await loadProof();
    setView(action === "home" ? "home" : action);
    return;
  }
  if (action === "start-roster") {
    await joinQueue();
    return;
  }
  if (action === "connect") {
    await connectWallet();
    return;
  }
  if (action === "replay") {
    await loadReplay();
    setView("replay");
    return;
  }
  if (action === "replay-toggle") {
    state.replayPlaying ? stopReplay() : startReplay();
    if (!state.replayPlaying) render();
    return;
  }
  if (action === "toggle-basis") {
    state.basisOpen = !state.basisOpen;
    render();
  }
});

document.addEventListener("change", (event) => {
  const assetId = event.target.dataset.assetId;
  if (assetId) {
    const id = Number(assetId);
    if (event.target.checked && state.selectedAssets.size < 6) state.selectedAssets.add(id);
    if (!event.target.checked) state.selectedAssets.delete(id);
    if (state.selectedAssets.size === 6 && !state.selectedAssets.has(state.captain)) state.captain = [...state.selectedAssets][0];
    render();
    return;
  }
  if (event.target.id === "captain-select") {
    state.captain = Number(event.target.value);
    render();
    return;
  }
  if (event.target.id === "replay-range") {
    state.replayIndex = Number(event.target.value);
    render();
  }
});

window.addEventListener("keydown", (event) => {
  if (event.key === "w" && (event.ctrlKey || event.metaKey)) return;
  if (event.key === "c" && (event.ctrlKey || event.metaKey)) return;
});

render();
hydrateBackend();
