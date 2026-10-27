# Session 20261026T220345Z (measure) coverage by UTC hour (EXPLORATORY)

2026-10-26T22:03:46Z to 2026-10-27T04:03:46Z. Covered 6.0 h. Hours flagged GAP had no coverage; nothing is filled.

| hour (UTC) | session min | covered min | blind (min, by kind) | windows | reserved lamports | median abs clock err (ms) | flag |
|---|---|---|---|---|---|---|---|
| 2026-10-26T22:00:00Z | 56 | 56 |  | 30 | 947860 | 80 | OK |
| 2026-10-26T23:00:00Z | 60 | 60 |  | 31 | 980876 | 93 | OK |
| 2026-10-27T00:00:00Z | 60 | 60 |  | 32 | 1008460 | 88 | OK |
| 2026-10-27T01:00:00Z | 60 | 60 |  | 31 | 983418 | 70 | OK |
| 2026-10-27T02:00:00Z | 60 | 60 |  | 31 | 1016260 | 109 | OK |
| 2026-10-27T03:00:00Z | 60 | 60 |  | 32 | 978160 | 79 | OK |
| 2026-10-27T04:00:00Z | 4 | 2 |  | 1 | 31658 | 360 | THIN |

Reserved in the ledger during the session: 5946692 lamports (cap 5960738). Per hour above; expired reservations count, so this is an upper bound of the fees paid.

## Windows skipped, by reason

| reason | count |
|---|---|
| pacing: # + # > # (plus one window) | 3236 |
| allocation quota met | 1286 |
| too close to previous planned window | 174 |
| window truncated by schedule range | 41 |
| planning budget: # + # > # | 5 |
| lead # ms < min # | 2 |
| getFeeForMessage disagrees with the fee model | 1 |

## Network path (Wi-Fi limitation)

         RTT to the RPC endpoint (getSlot, proxy for the path), 4238 probes, 0 failed: min 43.9 ms,          median 52.0, p95 118.3, p99 128.8, max 859.3; robust sigma 4.6 ms; jitter (mean |diff| of consecutive          probes) 19.6 ms.

         Wi-Fi: network types {"wifi":707}, bands {"2.4 GHz":707}, channels {"1":707}, PHY {"802.11n":707}, 1 access point(s), 0 change(s).          Signal quality min 92 / median 100 / max 100 %.
