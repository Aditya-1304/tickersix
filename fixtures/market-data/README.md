# Market-data baseline fixtures

These files are schema fixtures for the permanent Jupiter/xStocks baseline.
They are intentionally not claimed as live provider evidence and must not be
used as rated settlement data.

The xStocks v2 public contract exposes asset identity, price data, current and
pending multiplier values, and upcoming corporate actions without requiring an
API key. The current asset payload can omit the asset deployment's own token
program field, so `solana-token-program.json` records the separate Solana mint
owner verification needed to establish `Token2022Program`. A live smoke run
should replace or supplement these fixtures with timestamped provider
responses before Gate 0A is declared operationally complete.
