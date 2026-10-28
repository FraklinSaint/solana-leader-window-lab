# Session 20261028T165830Z (measure) coverage by UTC hour (EXPLORATORY)

2026-10-28T16:58:31Z to 2026-10-28T20:58:31Z. Covered 4.0 h. Hours flagged GAP had no coverage; nothing is filled.

| hour (UTC) | session min | covered min | blind (min, by kind) | windows | reserved lamports | median abs clock err (ms) | flag |
|---|---|---|---|---|---|---|---|
| 2026-10-28T16:00:00Z | 1 | 1 |  | 2 | 65858 | 57 | OK |
| 2026-10-28T17:00:00Z | 60 | 60 |  | 43 | 1359240 | 74 | OK |
| 2026-10-28T18:00:00Z | 60 | 60 |  | 44 | 1390898 | 68 | OK |
| 2026-10-28T19:00:00Z | 60 | 60 |  | 44 | 1392082 | 81 | OK |
| 2026-10-28T20:00:00Z | 59 | 57 |  | 38 | 1203492 | 111 | OK |

Reserved in the ledger during the session: 5411570 lamports (cap 5534647). Per hour above; expired reservations count, so this is an upper bound of the fees paid.

## Windows skipped, by reason

| reason | count |
|---|---|
| pacing: # + # > # (plus one window) | 2215 |
| allocation quota met | 767 |
| too close to previous planned window | 176 |
| window truncated by schedule range | 20 |
| QUIC connect failed: QUIC handshake timed out (# ms): deadline has elapsed | 1 |
| lead # ms < min # | 1 |

## Network path (Wi-Fi limitation)

         RTT to the RPC endpoint (getSlot, proxy for the path), 2830 probes, 0 failed: min 40.8 ms,          median 51.7, p95 87.7, p99 132.1, max 1135.4; robust sigma 8.1 ms; jitter (mean |diff| of consecutive          probes) 16.7 ms.

         Wi-Fi: network types {"wifi":472}, bands {"2.4 GHz":472}, channels {"1":472}, PHY {"802.11n":472}, 1 access point(s), 0 change(s).          Signal quality min 92 / median 98 / max 100 %.
