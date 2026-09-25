import test from "node:test";
import assert from "node:assert/strict";

import {
  createWalletAdapter,
  discoverWallet,
  encodeBase58,
} from "./wallet-client.js";

const WALLET = "11111111111111111111111111111111";

test("encodes wallet signatures as Solana base58", () => {
  assert.equal(encodeBase58(new Uint8Array([0, 1, 2, 255])), "1LiA");
});

test("adapts a legacy Solana provider without exposing a private key", async () => {
  const provider = {
    async connect() {
      return { publicKey: WALLET };
    },
    async signMessage(message) {
      assert.equal(new TextDecoder().decode(message), "challenge");
      return { signature: new Uint8Array([1, 2, 3]) };
    },
    async disconnect() {},
  };

  const wallet = createWalletAdapter(provider);
  assert.equal(await wallet.connect(), WALLET);
  assert.equal(await wallet.signMessage("challenge"), "Ldp");
  await wallet.disconnect();
});

test("discovers and signs through the Wallet Standard registration event", async () => {
  const wallet = {
    name: "Mock Standard Wallet",
    chains: ["solana:devnet"],
    accounts: [{ address: WALLET }],
    features: {
      "solana:signMessage": {
        async signMessage({ account, message }) {
          assert.equal(account.address, WALLET);
          assert.equal(new TextDecoder().decode(message), "challenge");
          return [{ signature: new Uint8Array([1, 2, 3]) }];
        },
      },
    },
  };
  const listeners = new Map();
  const globalObject = {
    addEventListener(type, listener) {
      listeners.set(type, listener);
    },
    dispatchEvent(event) {
      if (event.type === "wallet-standard:app-ready") {
        listeners.get("wallet-standard:register-wallet")?.({
          detail: ({ register }) => register(wallet),
        });
      }
      return true;
    },
    CustomEvent: class CustomEvent {
      constructor(type, init) {
        this.type = type;
        this.detail = init.detail;
      }
    },
  };

  const adapter = discoverWallet(globalObject);
  assert.ok(adapter);
  assert.equal(await adapter.connect(), WALLET);
  assert.equal(await adapter.signMessage("challenge"), "Ldp");
});

