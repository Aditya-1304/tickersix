/*
 * TickerSix's dependency-free consumer surface.
 *
 * The UI intentionally keeps network reads and local demo data behind the same
 * view model. That lets the hackathon demo run without a paid RPC, while a
 * deployed backend can provide the real round, proof, replay, and SSE data.
 * This client never receives private keys; wallet providers sign and send the
 * prepared commit transaction, while the browser only confirms its Devnet status.
 */

import { validateLineup } from "./lineup.mjs";
import { discoverWallet } from "./wallet-client.js";

const API_BASE = window.TICKERSIX_API_BASE || (window.location.port === "4173" ? "http://127.0.0.1:8788" : "");
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

const DEMO_PRIVATE_MARKETS = {
  competition_domain: "PRIVATE_MARKET",
  activation: "MetadataOnly",
  captured_at_unix: 0,
  exhibition: {
    eligible: false,
    status: "UNAVAILABLE",
    reason: "QUALITY_NOT_MEASURED",
    usable_reference_asset_count: 4,
    minimum_reference_asset_count: 6,
    competition_domain: "PRIVATE_MARKET",
    rated: false,
  },
  assets: [
    {
      id: "private-market-openai",
      reference_symbol: "OPENAI",
      display_name: "OPENAI",
      providers: ["PreStocks", "Tessera"],
      representation_count: 2,
      competition_domain: "PRIVATE_MARKET",
      exhibition_eligible: false,
    },
    {
      id: "private-market-anduril",
      reference_symbol: "ANDURIL",
      display_name: "ANDURIL",
      providers: ["PreStocks"],
      representation_count: 1,
      competition_domain: "PRIVATE_MARKET",
      exhibition_eligible: false,
    },
    {
      id: "private-market-kalshi",
      reference_symbol: "KALSHI",
      display_name: "KALSHI",
      providers: ["Tessera"],
      representation_count: 1,
      competition_domain: "PRIVATE_MARKET",
      exhibition_eligible: false,
    },
    {
      id: "private-market-spacex",
      reference_symbol: "SPACEX",
      display_name: "SPACEX",
      providers: ["Tessera"],
      representation_count: 1,
      competition_domain: "PRIVATE_MARKET",
      exhibition_eligible: false,
    },
  ],
};

const DEMO_PRIVATE_DETAILS = {
  "private-market-openai": {
    asset: DEMO_PRIVATE_MARKETS.assets[0],
    captured_at_unix: 0,
    exhibition: DEMO_PRIVATE_MARKETS.exhibition,
    representations: [
      {
        id: "private-market/prestocks/openai",
        provider: "PreStocks",
        reference_asset_id: "private-market-openai",
        reference_symbol: "OPENAI",
        representation_symbol: "OPENAI",
        display_name: "OpenAI PreStocks",
        structure_kind: "SpvEconomicExposure",
        provider_disclosure: "Provider-described SPV economic exposure; not ordinary shareholder rights",
        lifecycle_status: "UNSPECIFIED",
        source_url: "https://prestocks.com/openai",
        mint_or_contract: "PreStocks contract",
        mark_price_q9: 1001454006544,
        mark_valuation_q9: "1240731710609000000000",
        holder_count: null,
        comparability: "Unsupported",
        rated_settlement_eligible: false,
        competition_domain: "PRIVATE_MARKET",
      },
      {
        id: "private-market/tessera/t-openai",
        provider: "Tessera",
        reference_asset_id: "private-market-openai",
        reference_symbol: "OPENAI",
        representation_symbol: "T-OpenAI",
        display_name: "T-OpenAI",
        structure_kind: "LoanParticipationRight",
        provider_disclosure: "Provider-described loan participation right; not ordinary equity",
        lifecycle_status: "UNSPECIFIED",
        source_url: null,
        mint_or_contract: "Tessera mint",
        mark_price_q9: 812790000000,
        mark_valuation_q9: "950000000000000000000",
        holder_count: 8259,
        comparability: "Unsupported",
        rated_settlement_eligible: false,
        competition_domain: "PRIVATE_MARKET",
      },
    ],
  },
};

const DEMO_PROFILE = {
  wallet: "DemoWallet111111111111111111111111111111",
  display_name: "Aditya",
  season_id: 1,
  rating: 1601,
  rated_games: 7,
  peak_rating: 1620,
  wins: 5,
  draws: 0,
  losses: 2,
  placement_complete: true,
};

const DEMO_ACHIEVEMENTS = [
  {
    code: "FIRST_BLOOD",
    name: "First Blood",
    rarity: "COMMON",
    scope_type: "BATTLE",
    scope_id: DEMO_BATTLE,
    unlocked_at: 0,
    evidence: { rule: "first_fully_played_rated_win" },
  },
  {
    code: "PHOTO_FINISH",
    name: "Photo Finish",
    rarity: "UNCOMMON",
    scope_type: "BATTLE",
    scope_id: DEMO_BATTLE,
    unlocked_at: 0,
    evidence: { rule: "winning_margin_between_1_and_10_bps" },
  },
  {
    code: "GREEN_SIX",
    name: "Green Six",
    rarity: "RARE",
    scope_type: "BATTLE",
    scope_id: DEMO_BATTLE,
    unlocked_at: 0,
    evidence: { rule: "all_six_asset_returns_positive" },
  },
];

const state = {
  view: "home",
  round: DEMO_ROUND,
  assets: [],
  selectedAssets: new Set(),
  captain: null,
  assetsLoading: false,
  assetsError: null,
  commitBusy: false,
  commitStage: null,
  commitSignature: null,
  commitment: null,
  commitError: null,
  battlePubkey: DEMO_BATTLE,
  replay: DEMO_REPLAY,
  proof: DEMO_PROOF,
  replayIndex: DEMO_REPLAY.events.length - 1,
  replayPlaying: false,
  replayTimer: null,
  wallet: null,
  walletAdapter: null,
  authenticated: false,
  authExpiresAt: null,
  authBusy: false,
  authError: null,
  queueEntry: null,
  pairing: null,
  rankedStatusTimer: null,
  rankedStatusPollInFlight: false,
  leagues: [],
  leagueMemberships: {},
  leagueInstructions: {},
  leaguesLoading: false,
  leaguesError: null,
  leagueBusyId: null,
  profileSaving: false,
  queueBusy: false,
  backendOnline: false,
  toast: null,
  basisOpen: false,
  privateMarkets: DEMO_PRIVATE_MARKETS,
  privateMarketsError: null,
  privateMarketsLoading: false,
  privateAssetDetail: null,
  privateComparison: null,
  profile: DEMO_PROFILE,
  achievements: DEMO_ACHIEVEMENTS,
  profileError: null,
  profileLoading: false,
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
    const body = await response.json().catch(() => null);
    const error = new Error(body?.error || `HTTP ${response.status}`);
    error.status = response.status;
    error.code = body?.error || null;
    throw error;
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

async function loadPrivateMarkets() {
  state.privateMarketsLoading = true;
  state.privateMarketsError = null;
  state.privateAssetDetail = null;
  state.privateComparison = null;
  render();
  try {
    state.privateMarkets = await api("/v1/private-markets/assets");
    state.backendOnline = true;
  } catch (error) {
    state.privateMarkets = DEMO_PRIVATE_MARKETS;
    state.privateMarketsError = error.message || "PRIVATE_MARKETS_UNAVAILABLE";
  } finally {
    state.privateMarketsLoading = false;
    render();
  }
}

async function loadProfile() {
  state.profileLoading = true;
  state.profileError = null;
  render();
  if (!state.authenticated) {
    state.profile = DEMO_PROFILE;
    state.achievements = DEMO_ACHIEVEMENTS;
    state.profileLoading = false;
    render();
    return;
  }
  try {
    state.profile = await api("/v1/profile/me");
    state.wallet = state.profile.wallet;
    state.achievements = await api(
      `/v1/profiles/${encodeURIComponent(state.wallet)}/achievements`,
    );
    state.backendOnline = true;
  } catch (error) {
    if (error.status === 401) clearAuthenticatedState();
    state.profile = DEMO_PROFILE;
    state.achievements = DEMO_ACHIEVEMENTS;
    state.profileError = error.message || "PROFILE_UNAVAILABLE";
  } finally {
    state.profileLoading = false;
    render();
  }
}

async function updateProfile() {
  if (!state.authenticated) {
    showToast("Authenticate your wallet before editing your profile.");
    return;
  }
  const displayName = document.querySelector("#profile-display-name")?.value.trim() || null;
  const avatarUrl = document.querySelector("#profile-avatar-url")?.value.trim() || null;
  state.profileSaving = true;
  render();
  try {
    state.profile = await api("/v1/profile/me", {
      method: "PUT",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ display_name: displayName, avatar_url: avatarUrl }),
    });
    state.profileError = null;
    showToast("Profile updated.");
  } catch (error) {
    if (error.status === 401) clearAuthenticatedState();
    state.profileError = error.message || "PROFILE_UPDATE_FAILED";
    showToast(`Profile update failed: ${state.profileError}`);
  } finally {
    state.profileSaving = false;
    render();
  }
}

async function loadLeagues() {
  state.leaguesLoading = true;
  state.leaguesError = null;
  render();
  try {
    state.leagues = await api("/v1/leagues?status=REGISTRATION");
    state.backendOnline = true;
  } catch (error) {
    state.leagues = [];
    state.leaguesError = error.message || "LEAGUES_UNAVAILABLE";
  } finally {
    state.leaguesLoading = false;
    render();
  }
}

async function joinLeague(leagueId) {
  if (!state.authenticated) {
    await connectWallet();
    if (!state.authenticated) return;
  }
  state.leagueBusyId = leagueId;
  render();
  try {
    const response = await api(`/v1/leagues/${encodeURIComponent(leagueId)}/join`, {
      method: "POST",
    });
    state.leagueMemberships[leagueId] = response.membership;
    state.leagueInstructions[leagueId] = response.instruction;
    showToast("League join intent recorded; the coordinator must confirm the wallet instruction.");
  } catch (error) {
    if (error.status === 401) clearAuthenticatedState();
    showToast(`League join failed: ${error.message}`);
  } finally {
    state.leagueBusyId = null;
    render();
  }
}

async function leaveLeague(leagueId) {
  if (!state.authenticated) {
    showToast("Authenticate your wallet before leaving a league.");
    return;
  }
  state.leagueBusyId = leagueId;
  render();
  try {
    const response = await api(`/v1/leagues/${encodeURIComponent(leagueId)}/leave`, {
      method: "POST",
    });
    state.leagueMemberships[leagueId] = response.membership;
    state.leagueInstructions[leagueId] = response.instruction;
    showToast("League leave intent recorded; the coordinator must confirm the wallet instruction.");
  } catch (error) {
    if (error.status === 401) clearAuthenticatedState();
    showToast(`League leave failed: ${error.message}`);
  } finally {
    state.leagueBusyId = null;
    render();
  }
}


async function loadPrivateAsset(assetId) {
  state.privateMarketsError = null;
  try {
    state.privateAssetDetail = await api(
      `/v1/private-markets/assets/${encodeURIComponent(assetId)}`,
    );
    state.privateComparison = await api(
      `/v1/private-markets/comparisons/${encodeURIComponent(assetId)}`,
    );
  } catch (error) {
    state.privateAssetDetail = DEMO_PRIVATE_DETAILS[assetId] || null;
    state.privateComparison = state.privateAssetDetail
      ? {
          asset_id: assetId,
          reference_symbol: state.privateAssetDetail.asset.reference_symbol,
          status: "COMPARISON_UNAVAILABLE",
          reason: "PROVIDER_CLAIMS_NOT_CANONICALLY_COMPARABLE",
          numeric_basis_bps: null,
          competition_domain: "PRIVATE_MARKET",
          representations: state.privateAssetDetail.representations,
        }
      : null;
    state.privateMarketsError = error.message || "PRIVATE_MARKET_ASSET_UNAVAILABLE";
  }
  render();
}

function formatPrivatePrice(priceQ9) {
  if (priceQ9 === null || priceQ9 === undefined) return "—";
  return (Number(priceQ9) / 1_000_000_000).toFixed(2);
}

function privateStatusLabel(exhibition) {
  if (!exhibition) return "METADATA ONLY";
  return exhibition.eligible ? "EXHIBITION READY" : "METADATA ONLY";
}

function renderTopbar() {
  const authControls = state.authenticated
    ? `<span class="status-pill">AUTHENTICATED · ${escapeHtml(shortValue(state.wallet))}</span><button class="button ghost" data-action="logout">SIGN OUT</button>`
    : `<button class="button ghost" data-action="connect" ${state.authBusy ? "disabled" : ""}>${state.authBusy ? "SIGNING…" : "CONNECT & AUTHENTICATE"}</button>`;
  return `
    <header class="topbar">
      <button class="brand" data-action="home" aria-label="Return to TickerSix home">
        <span class="brand-mark">TS</span>
        <span>TICKERSIX</span>
      </button>
      <div class="hero-actions"><span class="cluster-pill">SOLANA DEVNET</span>${authControls}</div>
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
  const hasBattle = Boolean(state.pairing?.battle_pubkey);
  const hasPairing = Boolean(state.pairing);
  const queueLabel = hasBattle
    ? "MATCH FOUND"
    : hasPairing
      ? "OPPONENT FOUND"
      : state.queueEntry
        ? "SEARCHING"
        : "SCHEDULED";
  const action = hasBattle
    ? `<button class="button" data-action="build-lineup">BUILD LINEUP</button>`
    : state.queueEntry && !hasPairing
      ? `<button class="button secondary" data-action="leave-queue" ${state.queueBusy ? "disabled" : ""}>${state.queueBusy ? "LEAVING…" : "LEAVE QUEUE"}</button>`
      : "";

  const authStatus = state.authenticated
    ? `Authenticated as ${escapeHtml(shortValue(state.wallet))}. Queue mutations are tied to this session.`
    : "Connect and sign the authentication challenge to enter the ranked queue.";
  const pairingNotice = hasPairing
    ? `<div class="alert"><strong>${hasBattle ? "MATCH FOUND" : "OPPONENT FOUND"}:</strong> ${escapeHtml(shortValue(state.pairing.opponent))} · ${escapeHtml(String(state.pairing.opponent_rating))} rating · ${escapeHtml(state.pairing.status)}${hasBattle ? " · Battle confirmed" : " · waiting for Battle confirmation"}</div>`
    : state.queueEntry
      ? `<div class="alert"><strong>SEARCHING FOR OPPONENT:</strong> your queue admission is recorded for this MarketRound. The lineup builder opens only after the coordinator creates the Battle.</div>`
      : "";
  return `
    <section class="hero">
      <p class="eyebrow">Public Ranked</p>
      <h1>Queue for the next round.</h1>
      <p class="lede">Matchmaking is scheduled around one shared market window. There is no instant matchmaking and no hidden source switch.</p>
    </section>
    <article class="card">
      <div class="row"><div><h2>NEXT PUBLIC RANKED ROUND</h2><span class="muted">Round ${escapeHtml(state.round.round_sequence)} · frozen public-equity universe</span></div><span class="status-pill">${queueLabel}</span></div>
      <div class="source-line"><span class="source-pill">${escapeHtml(source.queue)}</span><span class="domain-pill">${escapeHtml(state.round.competition_domain)}</span><span class="cluster-pill">${escapeHtml(state.round.network)}</span></div>
      <div class="grid two">
        <div><div class="row"><span class="row-label">Round</span><strong>12:00–16:00 UTC</strong></div><div class="row"><span class="row-label">Queue closes</span><strong>11:45 UTC</strong></div></div>
        <div><div class="row"><span class="row-label">Lineups lock</span><strong>11:55 UTC</strong></div><div class="row"><span class="row-label">Rating</span><strong>Gold · 1584</strong></div></div>
      </div>
      <div class="alert"><strong>Source transparency:</strong> the provider and settlement source are frozen before queue admission. You cannot choose a different provider after freeze.</div>
      <div class="alert"><strong>Session:</strong> ${authStatus}</div>
      ${pairingNotice}
      <div class="hero-actions">${action}<button class="button secondary" data-action="home">BACK HOME</button></div>
      ${state.authError ? `<p class="muted" style="margin: 16px 0 0; font-size: 0.76rem">${escapeHtml(state.authError)}</p>` : ""}
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
      ${state.assetsLoading ? `<div class="empty-state">Loading the indexed RoundAsset universe…</div>` : ""}
      ${state.assetsError ? `<div class="alert"><strong>FROZEN ASSETS UNAVAILABLE:</strong> ${escapeHtml(state.assetsError)}</div>` : ""}
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
      <div class="hero-actions"><button class="button" data-action="review-lineup" ${selectedCount === 6 ? "" : "disabled"}>REVIEW LINEUP</button><button class="button secondary" data-action="queue">BACK TO QUEUE</button></div>
    </article>`;
}

function renderLineupReview() {
  const lineup = [...state.selectedAssets]
    .map((assetId) => state.assets.find((asset) => asset.id === assetId))
    .filter(Boolean);
  const validation = validateLineup(state.selectedAssets, state.captain, state.assets);
  const lockDeadline = state.round.commit_deadline || state.round.start_target_at;
  const lockLabel = lockDeadline
    ? new Date(lockDeadline * 1000).toISOString()
    : "pending from the indexed Battle";
  const canSend = Boolean(state.walletAdapter?.canSendTransactions);
  const canLock = validation.valid && state.authenticated && canSend && !state.commitBusy && state.commitStage !== "locked";
  const stageLabels = {
    preparing: "Preparing commitment…",
    waiting: "Waiting for wallet…",
    submitting: "Submitting transaction…",
    confirming: "Confirming on Devnet…",
    locked: "LINEUP LOCKED ✓",
    error: "Commit failed",
  };
  const stageNotice = state.commitStage
    ? "<div class=\"alert\"><strong>" + escapeHtml(stageLabels[state.commitStage] || state.commitStage) + "</strong>" + (state.commitError ? "<br />" + escapeHtml(state.commitError) : "") + "</div>"
    : "";
  const transactionNotice = state.commitSignature
    ? "<div class=\"proof-grid\"><div class=\"proof-item\"><small>Transaction</small><a href=\"https://explorer.solana.com/tx/" + encodeURIComponent(state.commitSignature) + "?cluster=devnet\" target=\"_blank\" rel=\"noreferrer\">" + escapeHtml(shortValue(state.commitSignature)) + " ↗</a></div><div class=\"proof-item\"><small>Commitment</small><code>" + escapeHtml(shortValue(state.commitment)) + "</code></div><div class=\"proof-item\"><small>Locked before</small><code>" + escapeHtml(lockLabel) + "</code></div></div>"
    : "";
  const capabilityNotice = state.authenticated && !canSend && state.commitStage !== "locked"
    ? "<div class=\"alert\"><strong>TRANSACTION WALLET REQUIRED:</strong> reconnect with a Devnet wallet that supports Wallet Standard signAndSendTransaction.</div>"
    : "";

  return `
    <section class="hero">
      <p class="eyebrow">Round ${escapeHtml(state.round.round_sequence)} · Lineup Review</p>
      <h1>Review before locking.</h1>
      <p class="lede">This is the exact lineup that will be committed for Battle ${escapeHtml(shortValue(state.battlePubkey))}. Once locked, it cannot be changed.</p>
    </section>
    <article class="card">
      <div class="row"><div><h2>YOUR SIX</h2><span class="muted">${lineup.length} / 6 frozen assets selected</span></div><span class="status-pill">${state.commitStage === "locked" ? "LINEUP LOCKED ✓" : validation.valid ? "READY TO LOCK" : "INCOMPLETE"}</span></div>
      <section class="grid three">
        ${lineup.map((asset) => `<article class="card stat-card"><span class="stat-label">${asset.id === state.captain ? "CAPTAIN · 2×" : "PICK"}</span><span class="stat-value">${escapeHtml(asset.symbol)}</span><span class="stat-subvalue">${escapeHtml(asset.name)}</span></article>`).join("")}
      </section>
      <div class="source-line"><span class="domain-pill">Round ${escapeHtml(String(state.round.round_sequence))}</span><span class="domain-pill">Lock deadline: ${escapeHtml(lockLabel)}</span><span class="domain-pill">${escapeHtml(state.round.settlement_source_kind)}</span></div>
      ${validation.valid ? "" : `<div class="alert"><strong>LINEUP NOT READY:</strong> ${escapeHtml(validation.reason)}</div>`}
      ${capabilityNotice}
      ${stageNotice}
      ${transactionNotice}
      <div class="hero-actions"><button class="button" data-action="lock-lineup" ${canLock ? "" : "disabled"}>${escapeHtml(state.commitStage === "locked" ? "LINEUP LOCKED ✓" : state.commitBusy ? "LOCKING…" : "LOCK LINEUP ON SOLANA")}</button><button class="button secondary" data-action="roster" ${state.commitBusy ? "disabled" : ""}>EDIT LINEUP</button></div>
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

function renderPrivateRepresentation(representation) {
  return `
    <article class="representation-card">
      <div class="row"><div><span class="mode-label">${escapeHtml(representation.provider)}</span><h3>${escapeHtml(representation.representation_symbol)}</h3></div><span class="status-pill">${escapeHtml(representation.lifecycle_status || "UNSPECIFIED")}</span></div>
      <div class="private-detail-grid">
        <div><span class="private-label">Structure</span><strong>${escapeHtml(representation.structure_kind)}</strong></div>
        <div><span class="private-label">Mark</span><strong>${formatPrivatePrice(representation.mark_price_q9)}</strong></div>
        <div><span class="private-label">Comparability</span><strong>${escapeHtml(representation.comparability)}</strong></div>
        <div><span class="private-label">Rated</span><strong>NO</strong></div>
      </div>
      <p class="card-copy">${escapeHtml(representation.provider_disclosure)}</p>
      ${representation.source_url ? `<a class="provider-link" href="${escapeHtml(representation.source_url)}" target="_blank" rel="noreferrer">VIEW PROVIDER SOURCE</a>` : `<span class="muted">PROVIDER SOURCE URL NOT PROVIDED</span>`}
    </article>`;
}

function renderPrivate() {
  const catalog = state.privateMarkets || DEMO_PRIVATE_MARKETS;
  const exhibition = catalog.exhibition || DEMO_PRIVATE_MARKETS.exhibition;
  const assets = catalog.assets || [];
  const detail = state.privateAssetDetail;
  const comparison = state.privateComparison;
  const fallbackNotice = state.privateMarketsError
    ? `<div class="alert"><strong>READ-ONLY FALLBACK:</strong> Provider/API data is unavailable (${escapeHtml(state.privateMarketsError)}). Public Ranked is unaffected; showing checked-in demo metadata only.</div>`
    : "";
  const assetCards = assets.length
    ? assets
        .map(
          (asset) => `<button class="private-asset-card" data-action="private-asset" data-asset-id="${escapeHtml(asset.id)}"><div class="row"><span class="asset-symbol">${escapeHtml(asset.reference_symbol)}</span><span class="status-pill">${escapeHtml(asset.representation_count)} REP${asset.representation_count === 1 ? "" : "S"}</span></div><span class="asset-name">${escapeHtml(asset.display_name)}</span><span class="asset-meta">${escapeHtml(asset.providers.join(" · "))}</span><span class="muted">${escapeHtml(asset.competition_domain)} · ${asset.exhibition_eligible ? "EXHIBITION READY" : "EXHIBITION UNAVAILABLE"}</span></button>`,
        )
        .join("")
    : `<div class="empty-state">No provider representations are currently available. The private surface is unavailable rather than fabricated.</div>`;
  const detailMarkup = detail
    ? `
      <article class="card private-detail">
        <div class="section-heading"><div><p class="eyebrow">Reference Asset</p><h2>${escapeHtml(detail.asset.reference_symbol)}</h2></div><button class="button ghost" data-action="private-clear">CLOSE</button></div>
        <div class="source-line"><span class="domain-pill">PRIVATE MARKET</span><span class="status-pill">${escapeHtml(privateStatusLabel(detail.exhibition))}</span></div>
        <div class="grid two">${detail.representations.map(renderPrivateRepresentation).join("")}</div>
        ${comparison ? `<div class="comparison-panel"><div class="row"><strong>Comparison</strong><span class="status-pill">${escapeHtml(comparison.status)}</span></div><p class="card-copy">${escapeHtml(comparison.reason)}. No numeric basis is displayed because the provider claims are not canonically comparable.</p></div>` : ""}
      </article>`
    : "";

  return `
    <section class="hero">
      <p class="eyebrow">Separate competition domain</p>
      <h1>Private Markets.</h1>
      <p class="lede">Provider context is useful. It is not permission to merge different economic claims into Public Equity Elo.</p>
    </section>
    ${fallbackNotice}
    <article class="card">
      <div class="source-line"><span class="domain-pill">${escapeHtml(catalog.competition_domain)}</span><span class="status-pill">${escapeHtml(privateStatusLabel(exhibition))}</span></div>
      <div class="row"><div><h2>Provider representations</h2><p class="card-copy">Captured metadata from PreStocks and Tessera. The card describes structure and lifecycle instead of implying ordinary company shares.</p></div><span class="muted">${assets.length} reference assets</span></div>
      <div class="alert"><strong>Rating isolation:</strong> Private Market results are exhibition-only by default. They cannot update Public Equity Elo, the global leaderboard, or competitive achievements.</div>
      <div class="exhibition-panel"><div><span class="private-label">Exhibition readiness</span><strong>${escapeHtml(exhibition.status)}</strong><span class="muted">${escapeHtml(exhibition.reason)} · ${escapeHtml(exhibition.usable_reference_asset_count)}/${escapeHtml(exhibition.minimum_reference_asset_count)} usable references</span></div><button class="button secondary" data-action="private-exhibition" ${exhibition.eligible ? "" : "disabled"}>${exhibition.eligible ? "OPEN EXHIBITION" : "GATE CLOSED"}</button></div>
      ${state.privateMarketsLoading ? `<div class="empty-state">Refreshing provider metadata…</div>` : `<div class="private-asset-grid">${assetCards}</div>`}
    </article>
    ${detailMarkup}
    <div class="proof-actions"><button class="button secondary" data-action="home">BACK TO PUBLIC RANKED</button></div>`;
}

function renderProfile() {
  const profile = state.profile || DEMO_PROFILE;
  const achievements = state.achievements || [];
  const errorNotice = state.profileError
    ? `<div class="alert"><strong>PROFILE NOTICE:</strong> ${escapeHtml(state.profileError)}. ${state.authenticated ? "The authenticated profile could not be refreshed." : "Showing the clearly labelled local demo profile."}</div>`
    : !state.authenticated
      ? `<div class="alert"><strong>DEMO PROFILE:</strong> Connect and authenticate a Devnet wallet to load and edit your real progress.</div>`
      : "";
  const profileEditor = state.authenticated
    ? `<article class="card">
        <div class="section-heading"><div><h2>Profile details</h2><p class="card-copy">These fields are cosmetic and are saved against the authenticated wallet session.</p></div><span class="status-pill">${escapeHtml(shortValue(state.wallet))}</span></div>
        <label class="field-label" for="profile-display-name">Display name</label>
        <input class="text-input" id="profile-display-name" maxlength="32" value="${escapeHtml(profile.display_name || "")}" placeholder="Your display name" />
        <label class="field-label" for="profile-avatar-url">Avatar URL</label>
        <input class="text-input" id="profile-avatar-url" maxlength="512" value="${escapeHtml(profile.avatar_url || "")}" placeholder="https://…" />
        <div class="proof-actions"><button class="button" data-action="save-profile" ${state.profileSaving ? "disabled" : ""}>${state.profileSaving ? "SAVING…" : "SAVE PROFILE"}</button></div>
      </article>`
    : "";
  return `
    <section class="hero">
      <p class="eyebrow">Public Equity · Cosmetic progress</p>
      <h1>Your progress.</h1>
      <p class="lede">Achievements are derived from finalized, official Battle and League history. They have no economic value and never change Elo.</p>
    </section>
    ${errorNotice}
    <section class="grid three">
      <article class="card stat-card"><span class="stat-label">Rating</span><span class="stat-value">${escapeHtml(profile.rating)}</span><span class="stat-subvalue">Peak ${escapeHtml(profile.peak_rating)} · ${escapeHtml(profile.rated_games)} rated Battles</span></article>
      <article class="card stat-card"><span class="stat-label">Record</span><span class="stat-value">${escapeHtml(profile.wins)}–${escapeHtml(profile.losses)}</span><span class="stat-subvalue">${escapeHtml(profile.draws)} draws · Season ${escapeHtml(profile.season_id || "—")}</span></article>
      <article class="card stat-card"><span class="stat-label">Unlocked</span><span class="stat-value">${achievements.length}</span><span class="stat-subvalue">Cosmetic titles only</span></article>
    </section>
    ${profileEditor}
    <div class="section-heading"><h2>Achievements</h2><span class="muted">Evidence retained by the backend</span></div>
    ${state.profileLoading ? `<div class="empty-state">Refreshing profile history…</div>` : achievements.length ? `<section class="achievement-grid">${achievements.map((achievement) => `
      <article class="card achievement-card">
        <div class="row"><span class="achievement-code">${escapeHtml(achievement.code)}</span><span class="status-pill">${escapeHtml(achievement.rarity)}</span></div>
        <h3>${escapeHtml(achievement.name)}</h3>
        <p class="card-copy">${escapeHtml(achievement.evidence?.rule || "Derived from finalized history")}</p>
        <span class="muted">${escapeHtml(achievement.scope_type)} · ${escapeHtml(shortValue(achievement.scope_id))}</span>
      </article>`).join("")}</section>` : `<div class="empty-state">No competitive achievements yet. Complete a fully played Public Ranked Battle to begin.</div>`}
    <div class="alert"><strong>REWARDS · COMING SOON.</strong> These titles are cosmetic. No tokens, SOL, cash, staking, or guaranteed payout is attached to an unlock.</div>`;
}

function renderLeague() {
  const cards = state.leagues.length
    ? state.leagues.map((league) => {
        const membership = state.leagueMemberships[league.id];
        const instruction = state.leagueInstructions[league.id];
        const activeMembership = membership && membership.membership_status !== "LEFT";
        return `<article class="card">
          <div class="row"><div><span class="mode-label">${escapeHtml(league.status)}</span><h2>${escapeHtml(league.name)}</h2></div><span class="status-pill">${escapeHtml(league.joined_players)}/${escapeHtml(league.max_players)}</span></div>
          <div class="source-line"><span class="domain-pill">${league.rated ? "RATED" : "UNRATED"}</span><span class="cluster-pill">${escapeHtml(league.total_rounds)} ROUNDS</span><span class="muted">Round ${escapeHtml(league.current_round)}</span></div>
          <p class="card-copy">Registration closes at ${escapeHtml(new Date(league.registration_close_at * 1000).toISOString())}. League membership reserves the scheduled rounds for this wallet.</p>
          <div class="hero-actions"><button class="button ${activeMembership ? "secondary" : ""}" data-action="${activeMembership ? "league-leave" : "league-join"}" data-league-id="${escapeHtml(league.id)}" ${state.leagueBusyId === league.id ? "disabled" : ""}>${state.leagueBusyId === league.id ? "WORKING…" : activeMembership ? "LEAVE LEAGUE" : "JOIN LEAGUE"}</button></div>
          ${membership ? `<div class="alert"><strong>${escapeHtml(membership.membership_status)}:</strong> The request is recorded. The coordinator must confirm the wallet instruction before on-chain membership becomes active.</div>` : ""}
          ${instruction ? `<div class="proof-item"><small>Wallet instruction data</small><code>${escapeHtml(shortValue(instruction.data_base58))}</code></div>` : ""}
        </article>`;
      }).join("")
    : `<div class="empty-state">${state.leaguesError ? `League catalog unavailable: ${escapeHtml(state.leaguesError)}` : "No registration leagues are currently open."}</div>`;
  return `
    <section class="hero">
      <p class="eyebrow">Competitive leagues · Solana Devnet</p>
      <h1>Play a season.</h1>
      <p class="lede">League join and leave requests are authenticated to your wallet. The backend returns the exact instruction view; on-chain membership changes only after coordinator confirmation.</p>
    </section>
    ${state.leaguesLoading ? `<div class="empty-state">Loading registration leagues…</div>` : `<section class="grid two">${cards}</section>`}`;
}


function renderView() {
  switch (state.view) {
    case "profile":
      return renderProfile();
    case "queue":
      return renderQueue();
    case "league":
      return renderLeague();
    case "roster":
      return renderRoster();
    case "lineup-review":
      return renderLineupReview();
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
    ["profile", "PROGRESS"],
    ["queue", "RANKED"],
    ["league", "LEAGUES"],
    ["private", "PRIVATE"],
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


function authDomain() {
  return window.TICKERSIX_AUTH_DOMAIN || window.location.hostname || "localhost";
}

function clearAuthenticatedState() {
  stopRankedStatusPolling();
  state.wallet = null;
  state.authenticated = false;
  state.authExpiresAt = null;
  state.queueEntry = null;
  state.pairing = null;
  state.profile = DEMO_PROFILE;
  state.achievements = DEMO_ACHIEVEMENTS;
}

async function restoreSession() {
  try {
    const profile = await api("/v1/profile/me");
    state.wallet = profile.wallet;
    state.authenticated = true;
    state.profile = profile;
    state.backendOnline = true;
    state.achievements = await api(`/v1/profiles/${encodeURIComponent(state.wallet)}/achievements`);
  } catch (error) {
    clearAuthenticatedState();
    if (error.status && error.status !== 401) state.authError = error.message;
  }
  render();
}

async function connectWallet() {
  if (state.authBusy) return;
  const adapter = discoverWallet(window);
  if (!adapter) {
    state.authError = "No Wallet Standard or compatible Solana provider was detected.";
    showToast("Install a Devnet wallet that can sign messages, then try again.");
    return;
  }

  state.authBusy = true;
  state.authError = null;
  state.walletAdapter = adapter;
  render();
  try {
    const wallet = await adapter.connect();
    const challenge = await api("/v1/auth/challenge", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ wallet, domain: authDomain() }),
    });
    const signature = await adapter.signMessage(challenge.message);
    const session = await api("/v1/auth/verify", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ wallet, nonce: challenge.nonce, signature, domain: challenge.domain }),
    });
    state.wallet = session.wallet;
    state.authenticated = true;
    state.authExpiresAt = session.expires_at;
    state.backendOnline = true;
    await loadProfile();
    showToast(`Wallet authenticated: ${shortValue(state.wallet)}`);
  } catch (error) {
    clearAuthenticatedState();
    state.authError = error.message || "WALLET_AUTHENTICATION_FAILED";
    showToast(`Wallet authentication failed: ${state.authError}`);
  } finally {
    state.authBusy = false;
    render();
  }
}

async function logoutWallet() {
  try {
    if (state.authenticated) await api("/v1/auth/logout", { method: "POST" });
  } catch {
    // Local state is cleared even when the API is offline.
  }
  try {
    await state.walletAdapter?.disconnect();
  } catch {
    // Provider disconnect is best-effort; it cannot keep the session active.
  }
  clearAuthenticatedState();
  state.walletAdapter = null;
  state.authError = null;
  showToast("Wallet signed out.");
}

async function loadRoundAssets() {
  state.assetsLoading = true;
  state.assetsError = null;
  state.assets = [];
  state.selectedAssets = new Set();
  state.captain = null;
  render();
  try {
    const universe = await api(`/v1/market-rounds/${encodeURIComponent(state.round.id)}/assets`);
    if (universe.market_round_id !== state.round.id || !Array.isArray(universe.assets)) {
      throw new Error("ROUND_ASSET_UNIVERSE_INVALID");
    }
    state.assets = universe.assets.map((asset) => ({
      ...asset,
      id: asset.asset_id,
    }));
    state.backendOnline = true;
    return state.assets.length > 0;
  } catch (error) {
    state.assets = [];
    state.assetsError = error.message || "ROUND_ASSETS_UNAVAILABLE";
    showToast(`Frozen assets unavailable: ${state.assetsError}`);
    return false;
  } finally {
    state.assetsLoading = false;
    render();
  }
}

function stopRankedStatusPolling() {
  if (state.rankedStatusTimer) window.clearInterval(state.rankedStatusTimer);
  state.rankedStatusTimer = null;
  state.rankedStatusPollInFlight = false;
}

async function pollRankedStatus() {
  if (!state.authenticated || !state.queueEntry || !state.round?.id || state.rankedStatusPollInFlight) return;
  state.rankedStatusPollInFlight = true;
  try {
    const status = await api(`/v1/ranked/status?market_round_id=${encodeURIComponent(state.round.id)}`);
    state.queueEntry = status.queue;
    state.pairing = status.pairing;
    if (status.pairing?.battle_pubkey) {
      state.battlePubkey = status.pairing.battle_pubkey;
      stopRankedStatusPolling();
      showToast("Match found. Build your lineup when ready.");
    }
    render();
  } catch (error) {
    if (error.status === 401) clearAuthenticatedState();
    else state.authError = error.message || "RANKED_STATUS_UNAVAILABLE";
    render();
  } finally {
    state.rankedStatusPollInFlight = false;
  }
}

function startRankedStatusPolling() {
  stopRankedStatusPolling();
  void pollRankedStatus();
  state.rankedStatusTimer = window.setInterval(() => void pollRankedStatus(), 3000);
}

async function joinQueue() {
  if (!state.authenticated) {
    await connectWallet();
    if (!state.authenticated) return;
  }
  state.queueBusy = true;
  state.pairing = null;
  state.authError = null;
  render();
  try {
    state.queueEntry = await api("/v1/ranked/queue", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ market_round_id: state.round.id }),
    });
    showToast("Queue admission accepted. Searching for an opponent.");
    setView("queue");
    startRankedStatusPolling();
  } catch (error) {
    if (error.status === 401) clearAuthenticatedState();
    showToast(`Queue admission failed: ${error.message}`);
    render();
  } finally {
    state.queueBusy = false;
    render();
  }
}

function bytesToHex(bytes) {
  return Array.from(bytes, (byte) => byte.toString(16).padStart(2, "0")).join("");
}

function rememberCommittedLineup(salt) {
  try {
    sessionStorage.setItem("tickersix.lineup.commit.v1", JSON.stringify({
      version: 1,
      battle_pubkey: state.battlePubkey,
      asset_ids: [...state.selectedAssets].sort((left, right) => left - right),
      captain_asset_id: state.captain,
      salt,
    }));
  } catch {
    // Session storage is an optimization for the manual reveal path; the
    // confirmed on-chain commitment remains the source of truth.
  }
}

async function confirmDevnetTransaction(signature) {
  const rpcUrl = window.TICKERSIX_SOLANA_RPC_URL || "https://api.devnet.solana.com";
  const deadline = Date.now() + 45_000;
  while (Date.now() < deadline) {
    const response = await fetch(rpcUrl, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({
        jsonrpc: "2.0",
        id: 1,
        method: "getSignatureStatuses",
        params: [[signature], { searchTransactionHistory: true }],
      }),
    });
    if (!response.ok) throw new Error("DEVNET_CONFIRMATION_UNAVAILABLE");
    const payload = await response.json();
    const status = payload?.result?.value?.[0];
    if (status?.err) throw new Error("COMMIT_TRANSACTION_FAILED");
    if (status?.confirmationStatus === "confirmed" || status?.confirmationStatus === "finalized") return;
    await new Promise((resolve) => window.setTimeout(resolve, 1500));
  }
  throw new Error("COMMIT_CONFIRMATION_TIMEOUT");
}

async function commitLineup() {
  if (state.commitBusy || state.commitStage === "locked") return;
  if (!state.authenticated) {
    await connectWallet();
    if (!state.authenticated) return;
  }
  if (!state.walletAdapter?.canSendTransactions) {
    state.commitStage = "error";
    state.commitError = "WALLET_TRANSACTION_UNSUPPORTED";
    showToast("Reconnect with a Devnet wallet that supports transaction sending.");
    render();
    return;
  }
  const validation = validateLineup(state.selectedAssets, state.captain, state.assets);
  if (!validation.valid) {
    showToast("Lineup commit failed: " + validation.reason);
    return;
  }

  state.commitBusy = true;
  state.commitStage = "preparing";
  state.commitError = null;
  state.commitSignature = null;
  state.commitment = null;
  render();
  const salt = bytesToHex(crypto.getRandomValues(new Uint8Array(32)));
  try {
    const prepared = await api("/v1/battles/" + encodeURIComponent(state.battlePubkey) + "/lineup/prepare", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({
        asset_ids: [...state.selectedAssets],
        captain_asset_id: state.captain,
        salt,
      }),
    });
    state.commitment = prepared.commitment;
    state.round = { ...state.round, commit_deadline: prepared.commit_deadline };
    state.commitStage = "waiting";
    render();

    state.commitStage = "submitting";
    render();
    const signature = await state.walletAdapter.sendTransaction(prepared.transaction.serialized_base64);
    state.commitSignature = signature;
    rememberCommittedLineup(salt);
    state.commitStage = "confirming";
    render();
    await confirmDevnetTransaction(signature);
    state.commitStage = "locked";
    showToast("LINEUP LOCKED ✓ Your commitment is confirmed on Devnet.");
  } catch (error) {
    if (error.status === 401) clearAuthenticatedState();
    state.commitStage = "error";
    state.commitError = error.message || "LINEUP_COMMIT_FAILED";
    showToast("Lineup commit failed: " + state.commitError);
  } finally {
    state.commitBusy = false;
    render();
  }
}

async function leaveQueue() {
  if (!state.authenticated) {
    showToast("Authenticate your wallet before leaving the queue.");
    return;
  }
  state.queueBusy = true;
  render();
  try {
    await api(`/v1/ranked/queue?market_round_id=${encodeURIComponent(state.round.id)}`, {
      method: "DELETE",
    });
    stopRankedStatusPolling();
    state.queueEntry = null;
    state.pairing = null;
    showToast("You left the ranked queue.");
    setView("queue");
  } catch (error) {
    if (error.status === 401) clearAuthenticatedState();
    showToast(`Queue exit failed: ${error.message}`);
  } finally {
    state.queueBusy = false;
    render();
  }
}


document.addEventListener("click", async (event) => {
  const target = event.target.closest("[data-action]");
  if (!target) return;
  const action = target.dataset.action;
  if (action === "private") {
    await loadPrivateMarkets();
    setView("private");
    return;
  }
  if (action === "league") {
    await loadLeagues();
    setView("league");
    return;
  }
  if (action === "profile") {
    await loadProfile();
    setView("profile");
    return;
  }
  if (action === "home" || action === "queue" || action === "roster" || action === "result" || action === "proof") {
    if (action === "proof") await loadProof();
    setView(action === "home" ? "home" : action);
    return;
  }
  if (action === "private-asset") {
    await loadPrivateAsset(target.dataset.assetId);
    return;
  }
  if (action === "private-clear") {
    state.privateAssetDetail = null;
    state.privateComparison = null;
    render();
    return;
  }
  if (action === "private-exhibition") {
    showToast("Private exhibition readiness is isolated and unrated; no Battle or Public Elo mutation is submitted by this client.");
    return;
  }
  if (action === "logout") {
    await logoutWallet();
    return;
  }
  if (action === "save-profile") {
    await updateProfile();
    return;
  }
  if (action === "leave-queue") {
    await leaveQueue();
    return;
  }
  if (action === "league-join") {
    await joinLeague(Number(target.dataset.leagueId));
    return;
  }
  if (action === "league-leave") {
    await leaveLeague(Number(target.dataset.leagueId));
    return;
  }
  if (action === "start-roster") {
    await joinQueue();
    return;
  }
  if (action === "review-lineup") {
    const validation = validateLineup(state.selectedAssets, state.captain, state.assets);
    if (!validation.valid) {
      showToast(`Lineup review failed: ${validation.reason}`);
      return;
    }
    setView("lineup-review");
    return;
  }
  if (action === "lock-lineup") {
    await commitLineup();
    return;
  }
  if (action === "build-lineup") {
    if (!state.pairing?.battle_pubkey) {
      showToast("Wait for the coordinator to create the Battle.");
      return;
    }
    state.battlePubkey = state.pairing.battle_pubkey;
    if (!await loadRoundAssets()) return;
    setView("roster");
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
    if (!state.selectedAssets.has(state.captain)) state.captain = [...state.selectedAssets][0] ?? null;
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
restoreSession();
