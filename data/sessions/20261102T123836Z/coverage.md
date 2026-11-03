# Session 20261102T123836Z (measure) coverage by UTC hour (EXPLORATORY)

2026-11-02T12:38:37Z to 2026-11-02T14:23:37Z. Covered 1.7 h. Hours flagged GAP had no coverage; nothing is filled.

| hour (UTC) | session min | covered min | blind (min, by kind) | windows | reserved lamports | median abs clock err (ms) | flag |
|---|---|---|---|---|---|---|---|
| 2026-11-02T12:00:00Z | 21 | 21 |  | 21 | 664818 | 69 | OK |
| 2026-11-02T13:00:00Z | 60 | 59 | slot_feed 1 | 57 | 1804506 | 84 | OK |
| 2026-11-02T14:00:00Z | 24 | 24 |  | 14 | 443212 | 96 | OK |

Reserved in the ledger during the session: 2912536 lamports (cap 3225611). Per hour above; expired reservations count, so this is an upper bound of the fees paid.

## Windows skipped, by reason

| reason | count |
|---|---|
| allocation quota met | 628 |
| pacing: # + # > # (plus one window) | 481 |
| too close to previous planned window | 72 |
| window truncated by schedule range | 9 |
| lead # ms < min # | 1 |
| slot feed stale: last WebSocket notification # ms ago (limit # ms) | 1 |

## Network path (Wi-Fi limitation)

         RTT to the RPC endpoint (getSlot, proxy for the path), 1236 probes, 3 failed: min 43.8 ms,          median 53.4, p95 100.8, p99 125.9, max 3641.8; robust sigma 6.1 ms; jitter (mean |diff| of consecutive          probes) 26.7 ms.

         Wi-Fi: network types {"wifi":207}, bands {"2.4 GHz":207}, channels {"13":207}, PHY {"802.11n":207}, 1 access point(s), 0 change(s).          Signal quality min 84 / median 96 / max 100 %.
