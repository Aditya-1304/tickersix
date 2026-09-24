# TickerSix consumer app

This is the mobile-first consumer surface for Public Ranked, replay, proof, and the isolated Private Markets domain. It has no runtime dependency, no paid provider, and no RPC client. The client reads the backend API when it is available and falls back to clearly labelled local demo data when it is not.

The demo surface performs no wallet signing, transaction submission, settlement, rating update, or achievement update. The production queue mutation still requires the backend authentication/session flow.

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

Then open `http://127.0.0.1:4173`. Set `window.TICKERSIX_API_BASE` before loading the module if the API is hosted on another origin and CORS is configured for it.
