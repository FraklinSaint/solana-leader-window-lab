# Reconcile: external inflows per session

Generated 2026-10-29 from `scripts/explain_reconcile_gap.py` (read-only, stdlib; SHA-256
`eb982e80019d3000c48f5a5d1e368f41138c7db65fac388c240cd1bba36da44a`, the hash recorded in the LOGBOOK
on 2026-10-28 10:46; unchanged). Counts and lamports only.

## What the script covers

For two balance snapshots it takes **every** account in them (32 payers, the master and the sink: 34 accounts),
lists each account's finalized signatures with slot in (before slot, after slot] (`getSignaturesForAddress`, paged),
splits them into ledger txs (signature in `data/ledger.jsonl` as landed) and the rest, reads each tx's balance change
(saved block, else `getTransaction`) and checks per account that the wallet delta equals the sum of all balance changes
found (chain completeness), and globally `wallet delta + external inflow = ledger fees` (exit 1 otherwise). A tx outside the
ledger whose sender is our master or sink would be marked OURS (an unrecorded tx of ours); all inflows below are FOREIGN.
`sender reconcile` itself demands `wallet delta = ledger fees`, so it fails whenever a foreign inflow exists
(LOGBOOK 2026-10-28 10:46, "limitation").

The snapshots chain without a gap from 2026-10-26T21:53:04Z (slot 461823494) to 2026-10-29T10:00:14Z (slot 462839508): the
`after` snapshot of each row is the `before` snapshot of the next. Sessions 1 to 4 and the three top-up intervals between them were
all run on 2026-10-29 with the same script version; the earlier runs of sessions 1 and 2 (LOGBOOK 2026-10-28 10:46) give the same numbers.

## Per session (lamports)

Difference = ledger fees - (wallet delta + external inflow). Wallet delta is the fall of the sum of all wallet balances.

| Interval | Kind | Before snapshot (slot, UTC) | After snapshot (slot, UTC) | Ledger txs landed | Wallet delta | External inflow | Ledger fees | Difference |
|---|---|---|---|---|---|---|---|---|
| Session 1 | `20261026T220345Z`, run `live-20261026T220348Z` | 461823494 (2026-10-26T21:53:04Z) | 461988668 (2026-10-27T07:38:14Z) | 1006 | 5,299,338 | 0 | 5,299,338 | 0 |
| Top-up between sessions 1 and 2 | fund transfers only | 461988668 (2026-10-27T07:38:14Z) | 462227142 (2026-10-27T21:44:14Z) | 32 | 160,000 | 0 | 160,000 | 0 |
| Session 2 | `20261027T214439Z`, run `live-20261027T214442Z` | 462227142 (2026-10-27T21:44:14Z) | 462440747 (2026-10-28T10:22:12Z) | 1086 | 5,741,687 | 7,983 | 5,749,670 | 0 |
| Top-up between sessions 2 and 3 | fund transfers only | 462440747 (2026-10-28T10:22:12Z) | 462550959 (2026-10-28T16:53:03Z) | 32 | 160,000 | 0 | 160,000 | 0 |
| Session 3 | `20261028T165830Z`, run `live-20261028T165833Z` | 462550959 (2026-10-28T16:53:03Z) | 462621799 (2026-10-28T21:05:18Z) | 887 | 4,672,700 | 0 | 4,672,700 | 0 |
| Top-up between sessions 3 and 4 | fund transfers only | 462621799 (2026-10-28T21:05:18Z) | 462628155 (2026-10-28T21:27:56Z) | 32 | 160,000 | 0 | 160,000 | 0 |
| Session 4 | `20261028T213504Z`, run `live-20261028T213507Z` | 462628155 (2026-10-28T21:27:56Z) | 462839508 (2026-10-29T10:00:14Z) | 1253 | 6,453,905 | 155,271 | 6,609,176 | 0 |

Total external inflow over all seven intervals: 163,254 lamports in 6 transfers; no fee is missing and none is unrecorded.

## Each external inflow

All six had `err None`, sender not our master or sink, signature not in `data/ledger.jsonl`. Accounts are
shown by their first 8 characters (the full keys are in the snapshots).

| Interval | Our account | Signature | Sender | Slot | Amount |
|---|---|---|---|---|---|
| Session 2 | `CKfH7dxz` | `2L71H8EAnX1u2RPD9b3pzXkKBk9MZupgi9nXjmEhxtgHWz94pVoMzZ2tpp29yVZ8b6VCPxz3XPxJfPFQVGzuWDPq` | `7YpeDTcDZjBsvz9KFww9YLiSLssUsN6bbcHAkiaUy2iZ` | 462232791 | 7,983 |
| Session 4 | `22W9auAD` | `599AB4z81qHjmqANobpX26hn8oQ8hgYxYbgBykz5NjWCQb6QouPNTxjYN7keuopAJDCB5wefHCBQFL7MfbUNjygB` | `Cjd9NGyzx2U9jx8BFxH2AsQrKX4YPwGU2aCx4kPonN1m` | 462630332 | 10,068 |
| Session 4 | `6dX7HHru` | `4ewFjNUoqtm1UwbhgjCjqC4iQ1UVi8NBp95JyyhgAN8aLFYsRUTrWeqr1BnJEdpFeWpknCsZ4dEcvpQVJhYsqjp1` | `GaneYDwN2UjLTM2GETr7GWPxwDi7SDQWzLU3xdkUtuCQ` | 462659504 | 41,870 |
| Session 4 | `Bset9Fvf` | `9JaF3danYW6maJLnGFQZ5xuZzMNt7mV4Ym6V5WwPSomM1pR5eq3X6q4d3FS8qpcMj3EpfqXWGKZp4sv56cbZ9MW` | `3iysMXr11pYxHgxkZn973sShgNxMyh5P7RFknFnbHbMa` | 462628159 | 7,585 |
| Session 4 | `HTim7hEV` | `5e6Hry58DtUDSrVv7HCBZRjL76HMe2hRabFYcSuSB3KcCwqJ4fzvpobj6jDfvQKgg35hkjFwbUG8shFmmpx2EoLC` | `7pSPpAzw9Xi1Zh4o1tZgBHZ818zfJPpGJhEiDX1GNNC6` | 462659474 | 51,342 |
| Session 4 | `PGnjdZAc` | `3H3q8gnygDL2wjhFWaeCPqtDufqca49A2JwFr9PZzH2B4r9esDWwYjAakvw7q3RWDBCxafkDJMr4DkbYtDm3pvAD` | `9EBYai84Y925AGhg6EpHCiNHggULk3w2kGft4TgMd7mL` | 462659504 | 44,406 |

Sessions 1 and 3 and the three top-up intervals have no external inflow, so plain `sender reconcile` passes there (and passed when run).
`sender reconcile` fails for session 2 (known since 2026-10-28) and session 4 (2026-10-29); nothing was changed to make it pass.
