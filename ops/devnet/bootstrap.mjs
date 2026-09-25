import crypto from "node:crypto";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

import * as anchor from "@coral-xyz/anchor";
import { Connection, Keypair, PublicKey, SystemProgram } from "@solana/web3.js";

import { buildRoundTimes, validateBootstrapConfig } from "./config.mjs";

const PROGRAM_ID = new PublicKey("8sehrRxpnLbvpzgJx8MqB5YApZAh69z5yVvdK1Zeyj6Z");
const SYSTEM_PROGRAM = SystemProgram.programId;
const DEFAULT_RPC = "https://api.devnet.solana.com";
const DEFAULT_KEYPAIR = path.join(process.env.HOME || "", ".config", "solana", "id.json");
const DEFAULT_IDL = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../../target/idl/tickersix.json");

const SEEDS = Object.freeze({
  config: Buffer.from("config"),
  settlementPolicy: Buffer.from("settlement-policy"),
  jupiterSourceConfig: Buffer.from("jupiter-source-config"),
  qualityPolicy: Buffer.from("quality-policy"),
  attestorSet: Buffer.from("attestor-set"),
  asset: Buffer.from("asset"),
  marketRound: Buffer.from("market-round"),
  roundAsset: Buffer.from("round-asset"),
});

function usage() {
  console.log([
    "Usage: node bootstrap.mjs --config <path> [--send] [--rpc <url>] [--keypair <path>] [--idl <path>]",
    "",
    "Without --send, the command only validates the input and prints the account/transaction plan.",
    "Submitting requires TICKERSIX_BOOTSTRAP_APPROVED=YES and a user-controlled Devnet keypair.",
  ].join("\\n"));
}

function expandHome(value) {
  return value.startsWith("~/") ? path.join(process.env.HOME || "", value.slice(2)) : value;
}

function parseArgs(argv) {
  const args = { send: false };
  for (let index = 0; index < argv.length; index += 1) {
    const value = argv[index];
    if (value === "--help" || value === "-h") {
      usage();
      process.exit(0);
    }
    if (value === "--send") {
      args.send = true;
      continue;
    }
    if (["--config", "--rpc", "--keypair", "--idl"].includes(value)) {
      const next = argv[index + 1];
      if (!next || next.startsWith("--")) throw new Error(value + " requires a value");
      args[value.slice(2)] = next;
      index += 1;
      continue;
    }
    throw new Error("unknown argument: " + value);
  }
  if (!args.config) throw new Error("--config is required");
  return args;
}

function readJson(filePath) {
  return JSON.parse(fs.readFileSync(filePath, "utf8"));
}

function loadKeypair(filePath) {
  const bytes = readJson(expandHome(filePath));
  if (!Array.isArray(bytes) || bytes.length !== 64) {
    throw new Error("keypair must be a Solana JSON secret-key array with 64 bytes");
  }
  return Keypair.fromSecretKey(Uint8Array.from(bytes));
}

function u16(value) {
  const bytes = Buffer.alloc(2);
  bytes.writeUInt16LE(value);
  return bytes;
}

function u32(value) {
  const bytes = Buffer.alloc(4);
  bytes.writeUInt32LE(value);
  return bytes;
}

function u64(value) {
  const bytes = Buffer.alloc(8);
  bytes.writeBigUInt64LE(BigInt(value));
  return bytes;
}

function pda(...parts) {
  return PublicKey.findProgramAddressSync(parts, PROGRAM_ID)[0];
}

function publicKey(value, label) {
  try {
    return new PublicKey(value);
  } catch (error) {
    throw new Error(label + " is not a valid Solana public key: " + error.message);
  }
}

function hashArray(value) {
  return Array.from(crypto.createHash("sha256").update(JSON.stringify(value)).digest());
}

function symbolBytes(symbol) {
  const bytes = Buffer.from(symbol, "utf8");
  const result = Buffer.alloc(8);
  bytes.copy(result, 0, 0, 8);
  return Array.from(result);
}

function enumValue(name) {
  return { [name]: {} };
}

function accountMeta(pubkey) {
  return { pubkey, isWritable: false, isSigner: false };
}

async function accountOwner(connection, address) {
  const info = await connection.getAccountInfo(address, "confirmed");
  return info?.owner || null;
}

async function run() {
  const args = parseArgs(process.argv.slice(2));
  if (args.send && process.env.TICKERSIX_BOOTSTRAP_APPROVED !== "YES") {
    throw new Error("--send requires TICKERSIX_BOOTSTRAP_APPROVED=YES after reviewing the dry-run plan");
  }

  const configPath = path.resolve(expandHome(args.config));
  const config = validateBootstrapConfig(readJson(configPath));
  const rpcUrl = args.rpc || process.env.TICKERSIX_DEVNET_RPC_URL || DEFAULT_RPC;
  const keypairPath = expandHome(args.keypair || process.env.TICKERSIX_DEPLOY_WALLET || DEFAULT_KEYPAIR);
  const idlPath = path.resolve(expandHome(args.idl || process.env.TICKERSIX_IDL_PATH || DEFAULT_IDL));
  const payer = loadKeypair(keypairPath);
  const connection = new Connection(rpcUrl, "confirmed");
  const idl = readJson(idlPath);
  if (idl.address && idl.address !== PROGRAM_ID.toBase58()) {
    throw new Error("IDL address does not match the configured TickerSix program ID");
  }
  const wallet = new anchor.Wallet(payer);
  const provider = new anchor.AnchorProvider(connection, wallet, {
    commitment: "confirmed",
    preflightCommitment: "confirmed",
  });
  const program = new anchor.Program(idl, provider);
  const normalizedAttestors = config.attestors.map((value, index) => publicKey(value, "attestors[" + index + "]"));
  const normalizedAssets = config.assets.map((asset, index) => ({
    ...asset,
    scoringMint: publicKey(asset.scoringMint, "assets[" + index + "].scoringMint"),
  }));
  const configPda = pda(SEEDS.config);
  const attestorSetPda = pda(SEEDS.attestorSet, u16(1));
  const sourceConfigPda = pda(SEEDS.jupiterSourceConfig, u16(1));
  const policyPda = pda(SEEDS.settlementPolicy, u16(1));
  const qualityPolicyPda = pda(SEEDS.qualityPolicy, u16(1));
  const roundPda = pda(SEEDS.marketRound, u64(config.roundId));
  const registryPdas = normalizedAssets.map((asset) => pda(SEEDS.asset, u32(config.registryVersion), u16(asset.assetId)));
  const roundAssetPdas = normalizedAssets.map((asset) => pda(SEEDS.roundAsset, roundPda.toBuffer(), u16(asset.assetId)));
  const times = buildRoundTimes(Math.floor(Date.now() / 1000), config.timing);
  const policyHash = hashArray({ kind: "JUPITER_TOKEN_SPOT_V1", observationWindowSecs: config.timing.observationWindowSecs });
  const qualityHash = hashArray({ kind: "PUBLIC_EQUITY", minEligibleAssets: normalizedAssets.length, registryVersion: config.registryVersion });
  const eligibilityHash = hashArray({ roundId: config.roundId, registryVersion: config.registryVersion, assets: normalizedAssets.map((asset) => ({ assetId: asset.assetId, scoringMint: asset.scoringMint.toBase58() })) });
  const transactions = [];

  console.log("cluster=devnet");
  console.log("rpc=" + rpcUrl);
  console.log("program=" + PROGRAM_ID.toBase58());
  console.log("payer=" + payer.publicKey.toBase58());
  console.log("round=" + roundPda.toBase58() + " sequence=" + config.roundId);
  console.log("assets=" + normalizedAssets.length + " attestors=" + normalizedAttestors.length);
  console.log(JSON.stringify({ timestamps: times }, null, 2));

  async function submit(label, address, build) {
    const owner = await accountOwner(connection, address);
    if (owner && !owner.equals(PROGRAM_ID)) {
      throw new Error(label + " already exists but is not owned by the TickerSix program: " + address.toBase58());
    }
    if (owner) {
      console.log("SKIP " + label + " exists at " + address.toBase58());
      return null;
    }
    if (!args.send) {
      console.log("PLAN " + label + " -> " + address.toBase58());
      return null;
    }
    const signature = await build().rpc({ skipPreflight: false, commitment: "confirmed" });
    await connection.confirmTransaction(signature, "confirmed");
    transactions.push({ label, signature });
    console.log("SENT " + label + " " + signature);
    return signature;
  }

  const existingConfig = await program.account.config.fetchNullable(configPda);
  if (existingConfig) {
    if (!existingConfig.adminAuthority.equals(payer.publicKey) || !existingConfig.coordinatorAuthority.equals(payer.publicKey)) {
      throw new Error("existing config authority does not match the selected payer; refusing to mutate it");
    }
    if (Number(existingConfig.currentRegistryVersion) > config.registryVersion) {
      throw new Error("existing config has a newer registry version; choose a new operator configuration");
    }
  }

  await submit("initialize_config", configPda, () => program.methods.initializeConfig().accounts({
    payer: payer.publicKey,
    config: configPda,
    systemProgram: SYSTEM_PROGRAM,
  }));
  await submit("create_attestor_set", attestorSetPda, () => program.methods.createAttestorSet(
    1,
    normalizedAttestors,
  ).accounts({
    config: configPda,
    payer: payer.publicKey,
    attestorSet: attestorSetPda,
    systemProgram: SYSTEM_PROGRAM,
  }));
  await submit("create_jupiter_source_config", sourceConfigPda, () => program.methods.createJupiterSourceConfig(
    1,
    120,
    config.timing.sampleIntervalSecs,
    100,
    3,
    2,
    30,
  ).accounts({
    config: configPda,
    payer: payer.publicKey,
    jupiterSourceConfig: sourceConfigPda,
    attestorSet: attestorSetPda,
    systemProgram: SYSTEM_PROGRAM,
  }));
  await submit("create_settlement_policy", policyPda, () => program.methods.createSettlementPolicy(
    1,
    enumValue("jupiterTokenSpotV1"),
    config.timing.observationWindowSecs,
    120,
    config.timing.sampleIntervalSecs,
    100,
    3,
    2,
    30,
    policyHash,
    sourceConfigPda,
  ).accounts({
    config: configPda,
    payer: payer.publicKey,
    pricePolicy: policyPda,
    systemProgram: SYSTEM_PROGRAM,
  }));
  await submit("create_market_quality_policy", qualityPolicyPda, () => program.methods.createMarketQualityPolicy(
    1,
    qualityHash,
    normalizedAssets.length,
    enumValue("publicEquity"),
  ).accounts({
    config: configPda,
    payer: payer.publicKey,
    marketQualityPolicy: qualityPolicyPda,
    systemProgram: SYSTEM_PROGRAM,
  }));

  for (let index = 0; index < normalizedAssets.length; index += 1) {
    const asset = normalizedAssets[index];
    await submit("create_registry_entry asset=" + asset.assetId, registryPdas[index], () => program.methods.createRegistryEntry(
      config.registryVersion,
      asset.assetId,
      symbolBytes(asset.symbol),
      asset.scoringMint,
      asset.issuerKind,
      asset.pythFeedId,
    ).accounts({
      config: configPda,
      payer: payer.publicKey,
      asset: registryPdas[index],
      systemProgram: SYSTEM_PROGRAM,
    }));
  }

  const currentConfig = await program.account.config.fetchNullable(configPda);
  if (!currentConfig || !currentConfig.registryFrozen) {
    if (!args.send) {
      console.log("PLAN freeze_registry_version");
    } else {
      const signature = await program.methods.freezeRegistryVersion().accounts({
        config: configPda,
        admin: payer.publicKey,
      }).rpc({ skipPreflight: false, commitment: "confirmed" });
      await connection.confirmTransaction(signature, "confirmed");
      transactions.push({ label: "freeze_registry_version", signature });
      console.log("SENT freeze_registry_version " + signature);
    }
  } else {
    console.log("SKIP freeze_registry_version already frozen");
  }

  await submit("create_market_round_draft", roundPda, () => program.methods.createMarketRoundDraft(
    new anchor.BN(config.roundId),
    config.registryVersion,
    eligibilityHash,
    new anchor.BN(times.eligibilityFrozenAt),
    new anchor.BN(times.queueCloseAt),
    new anchor.BN(times.commitDeadline),
    new anchor.BN(times.revealDeadline),
    new anchor.BN(times.startTargetAt),
    new anchor.BN(times.endTargetAt),
    false,
  ).accounts({
    config: configPda,
    coordinator: payer.publicKey,
    marketRound: roundPda,
    pricePolicy: policyPda,
    jupiterSourceConfig: sourceConfigPda,
    marketQualityPolicy: qualityPolicyPda,
    attestorSet: attestorSetPda,
    systemProgram: SYSTEM_PROGRAM,
  }));

  const existingRoundAssets = roundAssetPdas.map(accountMeta);
  for (let index = 0; index < normalizedAssets.length; index += 1) {
    const asset = normalizedAssets[index];
    const builder = program.methods.addRoundAsset(
      asset.assetId,
      asset.scoringMint,
      asset.issuerKind,
      enumValue("jupiterTokenSpotV1"),
      1,
      1,
    ).accounts({
      config: configPda,
      marketRound: roundPda,
      coordinator: payer.publicKey,
      roundAsset: roundAssetPdas[index],
      registryEntry: registryPdas[index],
      pricePolicy: policyPda,
      jupiterSourceConfig: sourceConfigPda,
      marketQualityPolicy: qualityPolicyPda,
      systemProgram: SYSTEM_PROGRAM,
    }).remainingAccounts(existingRoundAssets.slice(0, index));
    await submit("add_round_asset asset=" + asset.assetId, roundAssetPdas[index], () => builder);
  }

  const freezeBuilder = program.methods.freezeMarketRound().accounts({
    config: configPda,
    marketRound: roundPda,
    pricePolicy: policyPda,
    jupiterSourceConfig: sourceConfigPda,
    marketQualityPolicy: qualityPolicyPda,
    attestorSet: attestorSetPda,
    coordinator: payer.publicKey,
  }).remainingAccounts(roundAssetPdas.map(accountMeta));
  if (args.send) {
    const roundState = await program.account.marketRound.fetchNullable(roundPda);
    if (!roundState || roundState.state.preparing !== undefined) {
      const signature = await freezeBuilder.rpc({ skipPreflight: false, commitment: "confirmed" });
      await connection.confirmTransaction(signature, "confirmed");
      transactions.push({ label: "freeze_market_round", signature });
      console.log("SENT freeze_market_round " + signature);
    } else {
      console.log("SKIP freeze_market_round state is already " + JSON.stringify(roundState.state));
    }
  } else {
    console.log("PLAN freeze_market_round -> " + roundPda.toBase58());
  }

  const manifest = {
    schema_version: 1,
    cluster: "devnet",
    program_id: PROGRAM_ID.toBase58(),
    config_pubkey: configPda.toBase58(),
    round: {
      chain_pubkey: roundPda.toBase58(),
      round_sequence: config.roundId,
      registry_version: config.registryVersion,
      state: "SCHEDULED",
      is_replay: false,
      competition_domain: "PUBLIC_EQUITY",
      settlement_source_kind: "JUPITER_TOKEN_SPOT_V1",
      ...times,
    },
    policies: {
      settlement_policy: policyPda.toBase58(),
      jupiter_source_config: sourceConfigPda.toBase58(),
      market_quality_policy: qualityPolicyPda.toBase58(),
      attestor_set: attestorSetPda.toBase58(),
      policy_hash: Buffer.from(policyHash).toString("hex"),
      quality_hash: Buffer.from(qualityHash).toString("hex"),
      eligibility_snapshot_hash: Buffer.from(eligibilityHash).toString("hex"),
    },
    assets: normalizedAssets.map((asset, index) => ({
      asset_id: asset.assetId,
      symbol: asset.symbol,
      name: asset.name,
      representation: asset.representation,
      provider: asset.provider,
      scoring_mint: asset.scoringMint.toBase58(),
      round_asset_pubkey: roundAssetPdas[index].toBase58(),
      registry_entry_pubkey: registryPdas[index].toBase58(),
      status: "ELIGIBLE",
    })),
    transactions,
  };

  if (args.send) {
    const outputDirectory = path.resolve(process.env.TICKERSIX_DEVNET_ROUND_ARTIFACT_DIR || "artifacts/devnet-round");
    fs.mkdirSync(outputDirectory, { recursive: true });
    const outputPath = path.join(outputDirectory, "round-" + config.roundId + "-manifest.json");
    fs.writeFileSync(outputPath, JSON.stringify(manifest, null, 2) + "\\n");
    console.log("manifest=" + outputPath);
  } else {
    console.log("No transactions submitted. Re-run with --send and TICKERSIX_BOOTSTRAP_APPROVED=YES after review.");
  }
}

const invokedPath = process.argv[1] ? pathToFileURL(path.resolve(process.argv[1])).href : "";
if (import.meta.url === invokedPath) {
  run().catch((error) => {
    console.error("Devnet bootstrap failed: " + error.message);
    process.exitCode = 1;
  });
}

export { run };
