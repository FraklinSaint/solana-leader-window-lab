# Session 20261028T213504Z (measure) coverage by UTC hour (EXPLORATORY)

2026-10-28T21:35:05Z to 2026-10-29T03:05:05Z. Covered 5.5 h. Hours flagged GAP had no coverage; nothing is filled.

| hour (UTC) | session min | covered min | blind (min, by kind) | windows | reserved lamports | median abs clock err (ms) | flag |
|---|---|---|---|---|---|---|---|
| 2026-10-28T21:00:00Z | 25 | 25 |  | 21 | 661928 | 36 | OK |
| 2026-10-28T22:00:00Z | 60 | 60 |  | 49 | 1549014 | 76 | OK |
| 2026-10-28T23:00:00Z | 60 | 60 |  | 48 | 1512098 | 81 | OK |
| 2026-10-29T00:00:00Z | 60 | 60 |  | 48 | 1526514 | 79 | OK |
| 2026-10-29T01:00:00Z | 60 | 60 |  | 46 | 1455398 | 86 | OK |
| 2026-10-29T02:00:00Z | 60 | 60 |  | 24 | 765712 | 105 | OK |
| 2026-10-29T03:00:00Z | 5 | 5 |  | 3 | 93616 | 88 | OK |

Reserved in the ledger during the session: 7564280 lamports (cap 8478099). Per hour above; expired reservations count, so this is an upper bound of the fees paid.

## Windows skipped, by reason

| reason | count |
|---|---|
| allocation quota met | 2215 |
| pacing: # + # > # (plus one window) | 1883 |
| too close to previous planned window | 241 |
| window truncated by schedule range | 29 |

## Network path (Wi-Fi limitation)

         RTT to the RPC endpoint (getSlot, proxy for the path), 3898 probes, 0 failed: min 43.9 ms,          median 53.3, p95 120.7, p99 136.6, max 1511.4; robust sigma 5.6 ms; jitter (mean |diff| of consecutive          probes) 23.3 ms.

         Wi-Fi: network types {"wifi":650}, bands {"2.4 GHz":650}, channels {"1":650}, PHY {"802.11n":650}, 1 access point(s), 0 change(s).          Signal quality min 94 / median 100 / max 100 %.
