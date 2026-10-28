# Session 20261027T214439Z (measure) coverage by UTC hour (EXPLORATORY)

2026-10-27T21:44:40Z to 2026-10-28T03:44:40Z. Covered 6.0 h. Hours flagged GAP had no coverage; nothing is filled.

| hour (UTC) | session min | covered min | blind (min, by kind) | windows | reserved lamports | median abs clock err (ms) | flag |
|---|---|---|---|---|---|---|---|
| 2026-10-27T21:00:00Z | 15 | 15 |  | 12 | 379722 | 102 | OK |
| 2026-10-27T22:00:00Z | 60 | 60 | slot_feed 0 | 47 | 1488240 | 84 | OK |
| 2026-10-27T23:00:00Z | 60 | 60 |  | 46 | 1455398 | 64 | OK |
| 2026-10-28T00:00:00Z | 60 | 60 |  | 47 | 1490782 | 72 | OK |
| 2026-10-28T01:00:00Z | 60 | 60 |  | 37 | 1168108 | 82 | OK |
| 2026-10-28T02:00:00Z | 60 | 60 |  | 36 | 1141534 | 64 | OK |
| 2026-10-28T03:00:00Z | 45 | 45 |  | 24 | 763170 | 73 | OK |

Reserved in the ledger during the session: 7886954 lamports (cap 8819555). Per hour above; expired reservations count, so this is an upper bound of the fees paid.

## Windows skipped, by reason

| reason | count |
|---|---|
| allocation quota met | 2514 |
| pacing: # + # > # (plus one window) | 2014 |
| too close to previous planned window | 228 |
| window truncated by schedule range | 34 |
| QUIC connect failed: QUIC handshake timed out (# ms): deadline has elapsed | 4 |

## Network path (Wi-Fi limitation)

         RTT to the RPC endpoint (getSlot, proxy for the path), 4262 probes, 0 failed: min 43.1 ms,          median 53.0, p95 121.0, p99 132.2, max 416.2; robust sigma 5.9 ms; jitter (mean |diff| of consecutive          probes) 20.7 ms.

         Wi-Fi: network types {"wifi":711}, bands {"2.4 GHz":711}, channels {"1":711}, PHY {"802.11n":711}, 1 access point(s), 0 change(s).          Signal quality min 88 / median 100 / max 100 %.
