## Power-balanced allocation (fills PREREGISTRATION §8)

> SIMULATION, N = 6, seed 20261022, 200 replications, medium nuisance.
> Usable rate P(≥ 5 of 6 land) = 0.886. W = smallest W with half-strength
> power ≥ 0.9 (grid [10, 15, 20, 25, 30, 40, 50, 60, 80]). X rounded up to a multiple of 5.

| group | tier | validators | arm | key rule | W usable / validator | attempted / validator | expected usable (total) | cost (lamports) | power exact | power half | target reached |
|---|---|---|---|---|---|---|---|---|---|---|---|
| Agave | required | 2 | P | R_RATIO_AGAVE | 40 | 46 | 80 | 2,912,536 | 1.00 | 0.94 | yes |
| Agave | required | 2 | X | R_FIFO | 20 | 23 | 40 | 1,456,268 | 1.00 | 0.91 | yes |
| Agave | required | 2 | R | R_RATIO_AGAVE | 40 | 46 | 80 | 3,146,400 | 1.00 | 0.94 | yes |
| Agave | required | 2 | C | R_RATIO_AGAVE | 15 | 17 | 30 | 1,076,372 | 1.00 | 0.95 | yes |
| Agave | required | 2 | F | TIE_ARRIVAL_LIFO | 30 | 34 | 60 | 2,060,400 | 1.00 | 0.91 | yes |
| JitoAgave | required | 2 | P | R_RATIO_AGAVE | 40 | 46 | 80 | 2,912,536 | 1.00 | 0.94 | yes |
| JitoAgave | required | 2 | X | R_FIFO | 20 | 23 | 40 | 1,456,268 | 1.00 | 0.91 | yes |
| JitoAgave | required | 2 | R | R_RATIO_AGAVE | 40 | 46 | 80 | 3,146,400 | 1.00 | 0.94 | yes |
| JitoAgave | required | 2 | C | R_RATIO_AGAVE | 15 | 17 | 30 | 1,076,372 | 1.00 | 0.95 | yes |
| JitoAgave | required | 2 | F | TIE_ARRIVAL_LIFO | 30 | 34 | 60 | 2,060,400 | 1.00 | 0.91 | yes |
| Firedancer | required | 2 | P | R_RATIO_FD | 15 | 17 | 30 | 1,076,372 | 1.00 | 0.91 | yes |
| Firedancer | required | 2 | X | R_FIFO | 15 | 17 | 30 | 1,076,372 | 1.00 | 0.92 | yes |
| Firedancer | required | 2 | R | R_RATIO_FD | 15 | 17 | 30 | 1,162,800 | 1.00 | 0.90 | yes |
| Firedancer | required | 2 | C | R_RATIO_FD | 15 | 17 | 30 | 1,076,372 | 1.00 | 0.94 | yes |
| Firedancer | required | 2 | F | TIE_ARRIVAL_LIFO | 15 | 17 | 30 | 1,030,200 | 1.00 | 0.96 | yes |
| HarmonicAgave | bonus | 1 | P | R_RATIO_AGAVE | 30 | 34 | 30 | 1,076,372 | 1.00 | 0.83 | **no (trimmed)** |
| HarmonicAgave | bonus | 1 | X | R_FIFO | 20 | 23 | 20 | 728,134 | 1.00 | 0.91 | yes |
| HarmonicAgave | bonus | 1 | C | R_RATIO_AGAVE | 15 | 17 | 15 | 538,186 | 1.00 | 0.95 | yes |
| HarmonicFiredancer | bonus | 1 | P | R_RATIO_FD | 15 | 17 | 15 | 538,186 | 1.00 | 0.91 | yes |
| HarmonicFiredancer | bonus | 1 | X | R_FIFO | 15 | 17 | 15 | 538,186 | 1.00 | 0.92 | yes |
| HarmonicFiredancer | bonus | 1 | C | R_RATIO_FD | 15 | 17 | 15 | 538,186 | 1.00 | 0.94 | yes |
| BamAgave | bonus | 1 | P | R_RATIO_AGAVE | 40 | 46 | 40 | 1,456,268 | 1.00 | 0.94 | yes |
| BamAgave | bonus | 1 | X | R_FIFO | 20 | 23 | 20 | 728,134 | 1.00 | 0.91 | yes |
| BamAgave | bonus | 1 | C | R_RATIO_AGAVE | 15 | 17 | 15 | 538,186 | 1.00 | 0.95 | yes |
| BamFiredancer | bonus | 1 | P | R_RATIO_FD | 15 | 17 | 15 | 538,186 | 1.00 | 0.91 | yes |
| BamFiredancer | bonus | 1 | X | R_FIFO | 15 | 17 | 15 | 538,186 | 1.00 | 0.92 | yes |
| BamFiredancer | bonus | 1 | C | R_RATIO_FD | 15 | 17 | 15 | 538,186 | 1.00 | 0.94 | yes |
| Rakurai | bonus | 1 | P | R_RATIO_AGAVE | 40 | 46 | 40 | 1,456,268 | 1.00 | 0.94 | yes |
| Rakurai | bonus | 1 | X | R_FIFO | 20 | 23 | 20 | 728,134 | 1.00 | 0.91 | yes |
| Rakurai | bonus | 1 | C | R_RATIO_AGAVE | 15 | 17 | 15 | 538,186 | 1.00 | 0.95 | yes |

- Confirmatory total: **37,743,052 lamports** (budget 38,000,000; within budget: True).
- Arm F for bonus groups included: False.
- Calibration ≤ 5,000,000; funding/sweep/retry reserve ≤ 5,000,000; total cap 48,000,000 (hard ceiling 50,000,000).

### Power curves (half-strength rule; N = 6)

| family/arm | rule | W=10 | W=15 | W=20 | W=25 | W=30 | W=40 | W=50 | W=60 | W=80 |
|---|---|---|---|---|---|---|---|---|---|---|
| agave/P | R_RATIO_AGAVE | 0.41 | 0.56 | 0.67 | 0.77 | 0.83 | 0.94 | 0.96 | 0.98 | 1.00 |
| agave/R | R_RATIO_AGAVE | 0.48 | 0.61 | 0.73 | 0.81 | 0.86 | 0.94 | 0.98 | 0.98 | 1.00 |
| agave/C | R_RATIO_AGAVE | 0.88 | 0.95 | 0.98 | 1.00 | 0.99 | 1.00 | 1.00 | 1.00 | 1.00 |
| agave/X | R_FIFO | 0.73 | 0.84 | 0.91 | 0.94 | 0.97 | 0.99 | 1.00 | 1.00 | 1.00 |
| agave/F | TIE_ARRIVAL_LIFO | 0.51 | 0.71 | 0.80 | 0.86 | 0.91 | 0.97 | 0.98 | 0.99 | 1.00 |
| fd2/P | R_RATIO_FD | 0.75 | 0.91 | 0.96 | 0.99 | 0.99 | 1.00 | 1.00 | 1.00 | 1.00 |
| fd2/R | R_RATIO_FD | 0.79 | 0.90 | 0.97 | 0.99 | 0.99 | 1.00 | 1.00 | 1.00 | 1.00 |
| fd2/C | R_RATIO_FD | 0.81 | 0.94 | 0.98 | 0.99 | 1.00 | 1.00 | 1.00 | 1.00 | 1.00 |
| fd2/X | R_FIFO | 0.78 | 0.92 | 0.95 | 0.98 | 0.99 | 1.00 | 1.00 | 1.00 | 1.00 |
| fd2/F | TIE_ARRIVAL_LIFO | 0.83 | 0.96 | 0.99 | 1.00 | 1.00 | 1.00 | 1.00 | 1.00 | 1.00 |

### Positive control: vanilla Agave, Arm C, R_RATIO_AGAVE, W = 15

95% predictive interval of the mean raw tau: low [0.87, 1.00], medium [0.70, 1.00], high [0.65, 1.00]
; **registered (union): [0.65, 1.00]**.
