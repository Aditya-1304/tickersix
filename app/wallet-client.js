/*
 * Browser wallet boundary for TickerSix authentication.
 *
 * The backend authenticates a wallet by verifying a base58 Ed25519 signature.
 * This module contains only provider discovery, message signing, and byte
 * encoding. It never receives, derives, persists, or transmits private keys.
 * Keeping this boundary independent of the view makes the auth flow testable
 * with mocked providers and keeps the static demo free of a paid SDK/RPC.
 */

const BASE58_ALPHABET = "123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";
const DEVNET_CHAIN = "solana:devnet";

function asBytes(value) {
  if (value instanceof Uint8Array) return value;
  if (value instanceof ArrayBuffer) return new Uint8Array(value);
  if (ArrayBuffer.isView(value)) return new Uint8Array(value.buffer, value.byteOffset, value.byteLength);
  throw new TypeError("wallet signature must be a byte array");
}

/** Encodes raw Solana bytes using the base58 representation expected by the API. */
export function encodeBase58(value) {
  const bytes = asBytes(value);
  if (bytes.length === 0) return "";

  let encoded = "";
  let number = 0n;
  for (const byte of bytes) number = number * 256n + BigInt(byte);
  while (number > 0n) {
    const remainder = Number(number % 58n);
    encoded = BASE58_ALPHABET[remainder] + encoded;
    number /= 58n;
  }
  for (const byte of bytes) {
    if (byte !== 0) break;
    encoded = BASE58_ALPHABET[0] + encoded;
  }
  return encoded;
}

function publicKeyToAddress(publicKey) {
  if (typeof publicKey === "string") return publicKey;
  if (publicKey?.toBase58) return publicKey.toBase58();
  if (publicKey?.toString && publicKey.toString() !== "[object Object]") {
    return publicKey.toString();
  }
  return encodeBase58(publicKey);
}

function signatureToBase58(result) {
  const candidate = Array.isArray(result) ? result[0] : result;
  const signature = candidate?.signature ?? candidate;
  if (typeof signature === "string") return signature;
  return encodeBase58(signature);
}

function standardChainSupported(wallet) {
  return !wallet.chains?.length || wallet.chains.includes(DEVNET_CHAIN) || wallet.chains.some((chain) => chain.startsWith("solana:"));
}

function isStandardWallet(wallet) {
  return Boolean(
    wallet?.accounts?.length &&
      standardChainSupported(wallet) &&
      wallet.features?.["solana:signMessage"]?.signMessage,
  );
}

/**
 * Creates the small provider interface consumed by the UI auth flow.
 *
 * Both legacy injected providers and Wallet Standard wallets are adapted to
 * the same connect/sign/disconnect contract. The adapter deliberately has no
 * transaction-sending method because this flow authenticates sessions only.
 */
export function createWalletAdapter(provider) {
  if (isStandardWallet(provider)) {
    return {
      kind: "wallet-standard",
      name: provider.name || "Wallet Standard provider",
      async connect() {
        return publicKeyToAddress(provider.accounts[0].address);
      },
      async signMessage(message) {
        const account = provider.accounts[0];
        const result = await provider.features["solana:signMessage"].signMessage({
          account,
          message: new TextEncoder().encode(message),
        });
        return signatureToBase58(result);
      },
      async disconnect() {
        const disconnect = provider.features?.["standard:disconnect"]?.disconnect;
        if (disconnect) await disconnect.call(provider);
      },
    };
  }

  if (!provider?.connect || !provider?.signMessage) return null;
  return {
    kind: "legacy-injected",
    name: provider.name || "Injected Solana provider",
    async connect() {
      const response = await provider.connect();
      return publicKeyToAddress(response?.publicKey || provider.publicKey);
    },
    async signMessage(message) {
      const result = await provider.signMessage(new TextEncoder().encode(message), "utf8");
      return signatureToBase58(result);
    },
    async disconnect() {
      if (provider.disconnect) await provider.disconnect();
    },
  };
}

/**
 * Discovers a Solana Wallet Standard provider from the browser registration
 * events, with a legacy injected-provider fallback for older wallet builds.
 */
export function discoverWallet(globalObject = globalThis) {
  const wallets = [];
  const register = (...registeredWallets) => {
    for (const wallet of registeredWallets) {
      if (!wallets.includes(wallet)) wallets.push(wallet);
    }
  };

  if (globalObject.addEventListener && globalObject.dispatchEvent) {
    globalObject.addEventListener("wallet-standard:register-wallet", (event) => {
      if (typeof event.detail === "function") event.detail({ register });
    });
    const EventConstructor = globalObject.CustomEvent || globalThis.CustomEvent;
    if (EventConstructor) {
      globalObject.dispatchEvent(
        new EventConstructor("wallet-standard:app-ready", { detail: { register } }),
      );
    }
  }

  const legacyQueue = globalObject.navigator?.wallets;
  if (Array.isArray(legacyQueue)) {
    for (const callback of legacyQueue) callback({ register });
  }

  const standardWallet = wallets.find(isStandardWallet);
  return createWalletAdapter(standardWallet || globalObject.solana);
}

