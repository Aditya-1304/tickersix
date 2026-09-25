# TickerSix consumer app

This is the mobile-first consumer surface for Public Ranked, replay, proof, and the isolated Private Markets domain. It has no runtime dependency, no paid provider, and no RPC client. The client reads the backend API when it is available and falls back to clearly labelled local demo data when it is not.

The app supports Wallet Standard-compatible message signing for login only. It never asks the wallet to send a transaction, stores private keys, or mutates settlement, rating, or achievement state. When the API is unavailable, the local profile and market data remain clearly labelled demo data.

## Local checks

```bash
npm run check
node --check app.js
```

## Local preview

From this directory, use any static file server:

```bash
python3 -m http.server 4173
```

Then open `http://localhost:4173`. If the API is hosted on another origin, configure the browser page with `window.TICKERSIX_API_BASE` and start the API with `TICKERSIX_WEB_ORIGIN=http://localhost:4173`. The API only enables credentialed CORS when that origin is explicitly configured.
