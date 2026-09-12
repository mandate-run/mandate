# Mandate reference sellers

Four x402-gated endpoints on Hedera testnet, settled through Blocky402,
serving live evidence from the Uniswap v3 subgraph on The Graph Network.

| Route | Capability | Tariff | Produces |
|---|---|---|---|
| `POST /screen` | screening facts | `0 + 200/pool` | `Screening` |
| `POST /events` | transaction-level events | `0 + 1500/pool-window` | `Transaction` |
| `POST /investigate` | screening + events | `2000 + 1200/pool` | `Report` |
| `POST /explain` | prose from a brief | `100/KB` | `Report` |

Amounts are atomic USDC units (6 decimals) on HTS token `0.0.429274`
(testnet). The tariff table mirrors the pinned manifest exactly, so a quote at
the ceiling always passes the buyer's `within_tariff` check.

## Run

```bash
cd sellers
npm install
GRAPH_API_KEY=<Subgraph Studio key> npm start          # default http://localhost:4021
```

Environment:

| Variable | Default | Meaning |
|---|---|---|
| `GRAPH_API_KEY` | — | Subgraph Studio API key for the Uniswap v3 subgraph gateway (required) |
| `SELLER_PORT` | `4021` | HTTP port |
| `SELLER_BASE_URL` | `http://localhost:4021` | Public base URL used in 402 resource info and discovery |
| `FACILITATOR_URL` | `https://api.testnet.blocky402.com` | Blocky402 facilitator |
| `SELLER_PAY_TO` | `0.0.7777777` | Recipient of the paid transfers |
| `FEE_PAYER` | `0.0.7162784` | Facilitator fee payer advertised in `/supported` |
| `SELLER_ASSET` | `0.0.429274` | Service asset (testnet USDC) |
| `SELLER_FAULTS` | — | Comma-separated fault switches (below) |
| `MODEL_API_KEY`, `MODEL_URL` | — | Optional model for explain prose; falls back to a template built only from claim values |
| `SELLER_DATA_DIR` | `./data/results` | Durable result storage |

## Wire format (x402 v2)

- Unpaid request → `402` with `PAYMENT-REQUIRED` (base64 JSON) header and body.
- Paid request → `PAYMENT-SIGNATURE` header (base64 PaymentPayload) with the
  `payment-identifier` extension.
- Success → `200` with the evidence body and a `PAYMENT-RESPONSE` header.
- A resend or retrieval carrying the same payment id is served from durable
  storage without a second verify or settlement; a matching id with a
  different request fingerprint is `409`.

Sellers verify through the facilitator, then do the work, then settle — a
failed handler never settles (spec section 13). A request above `max_units`
is refused with `413`.

## Fault switches

`SELLER_FAULTS=drop-response-after-settle` on the events route settles, then
drops the response; the buyer recovers it by retrieving with the original
payment (no second payment). `SELLER_FAULTS=quote-drift` makes the events
seller quote above its ceiling so the buyer refuses `OFF_TARIFF`.

## Data source

All evidence is live data from the Uniswap v3 subgraph
`5zvR82QoaXYFyDEKLZ9t6v9adgnptxYpKpSbxtgVENFV` on The Graph Network, queried
through the gateway with a Subgraph Studio API key. TVL is read at two block
heights (window start and end); mint, burn and swap counts, sums and event
facts are read from the window with pagination to completion or a
`truncated` marker. Each new purchase queries live data; only resends and
retrievals are served from storage.