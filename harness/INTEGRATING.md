# Integrating a service with Proctor

Proctor reads two artifacts as plain files: a JSON-lines journal and a SQLite
ledger. It does not link to, call, or otherwise depend on the service that
wrote them. This makes it useful for a service other than Mandate.

For task syntax and the complete measurement list, see the
[Proctor README](README.md).

## Journal contract

Write one JSON object per request the service handled, one object per line.
Every line must parse; an unreadable or malformed journal is an infrastructure
error rather than a pass.

| Field | Type | Meaning |
|---|---|---|
| `route` | string | endpoint hit |
| `payment_id` | string or `null` | set when the request carried a payment |
| `signed_payload_hash` | string or `null` | hash of the signed payment bytes |
| `settle_called` | boolean | whether settlement was attempted |
| `settle_ok` | boolean | whether settlement succeeded |
| `served_hash` | string or `null` | hash of the result served |
| `status` | number | HTTP status |
| `source` | string | where the result came from |

From this file Proctor measures `fixture_requests`, `fixture_routes`,
`fixture_requests_<route>`, `fixture_signed_payloads`,
`fixture_distinct_payloads`, `fixture_payment_ids`, `fixture_settle_calls`,
`fixture_settlements`, `fixture_served_from_store`, `fixture_dropped`,
`fixture_rejected`, `fixture_distinct_results`, `fixture_errors`, and,
before a journal exists, `fixture_journal_missing`.

## Ledger contract

The ledger is a SQLite database with an `authorizations` table. For the
`mandate_id` named by the task, it needs at least these columns:

| Column | Used to measure |
|---|---|
| `mandate_id` | select the task's run |
| `payment_id` | distinct payment identifiers |
| `signature` | distinct signed payloads |
| `payment_state` | `prepared`, `sent`, `settled`, `failed`, and `unresolved` counts |
| `amount` | held reservation total |
| `submissions` | total submission attempts |
| `retrievals` | total retrieval attempts |

Mandate's ledger also has `reservations` and `receipts` tables. Proctor
measures `ledger_authorizations`, `ledger_payment_ids`,
`ledger_signed_payloads`, `ledger_submissions`, `ledger_retrievals`,
`ledger_settled`, `ledger_failed`, `ledger_unresolved`, `ledger_sent`,
`ledger_prepared`, `ledger_receipts`, `ledger_receipts_published`, and
`ledger_held` from the ledger.

`reservations` and `receipts` are optional to a service's accounting model,
but the current reader queries both to produce `ledger_held` and the receipt
metrics. A non-Mandate ledger used as evidence must therefore provide
compatible (possibly empty) tables: `reservations(mandate_id, amount, state)`
and `receipts(mandate_id, hcs_sequence)`.

## Why both sources matter

`expect` asserts against the application's own JSON report. `observe` asserts
against the journal and ledger Proctor read itself. A check passes only when
both agree. The split catches, for example, a service reporting a successful
retry while the journal records two signed payments or two settlements.

## Worked example: a paid weather API

This illustrative contract checks a `GET /weather` retry. The service reports
one delivered forecast, while its artifacts must show that the retry reused
the original payment rather than charging again. The literal paths are part
of the contract; Proctor expands no variables.

```toml
[task]
name = "weather retry is charged once"

[evidence]
journal = "evidence/weather.jsonl"
ledger = "evidence/weather.sqlite"
mandate_id = "weather-retry-001"

[[check]]
name = "retry uses the stored forecast"
run = "./scripts/check-weather-retry"
expect = { status = "delivered" }
observe = { fixture_requests_weather = 2, fixture_distinct_payloads = 1, fixture_settlements = 1, fixture_served_from_store = 1, ledger_authorizations = 1, ledger_settled = 1 }
```

The script must print the service report as JSON because `expect` reads its
stdout. The journal and ledger are what make the no-double-charge assertion
independent of that report.

## Limits

`proctor run` drives an agent adapter, so it needs one configured in the task;
`proctor check` does not. Task contracts hold literal paths and expand no
variables. The shipped tasks assume port 4021, so a different port also
requires matching evidence paths in the task.
