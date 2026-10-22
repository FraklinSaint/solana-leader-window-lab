"""SIMULATION: power-balanced allocation of windows to (validator group x arm).

``python -m slwlab.allocate --out sim/results/alloc_v1.json`` (N = 6, seed recorded).

Procedure (pre-registered; re-run after calibration with estimated nuisance, see
docs/PREREGISTRATION.md §0):
1. For each family (agave, fd2) and arm, simulate the arm's key rule exactly and at half
   strength over a fine W grid. Power = P(rule significant after Holm, one-sided 0.05).
2. W(cell) = the smallest grid W with half-strength power >= 0.9. If no grid W reaches it,
   W = the largest grid W and the cell is marked "power target unreachable". Arm X W is
   rounded up to a multiple of 5, so every send gap gets the same count.
3. Required groups (Agave, JitoAgave, Firedancer; 2 validators each) get arms P, X, R, C, F.
   Bonus groups (HarmonicAgave, HarmonicFiredancer, BamAgave, BamFiredancer, Rakurai; 1 each)
   get P, X, C. F is added to bonus groups only if the confirmatory budget still allows it.
4. Attempted windows = W / usable rate (P(>= 5 of 6 land) at the nuisance landing
   probability). Cost = attempted x the arm's per-window fee.
5. If the total exceeds the confirmatory budget, bonus cells are trimmed one W-grid step
   at a time, the most expensive first, until it fits. Trimmed cells are marked
   `trimmed: true` and report their (lower) power. Required cells are never trimmed. If
   trimming every bonus cell to W = 10 is not enough, the allocation is reported as
   over budget.
"""

from __future__ import annotations

import argparse
import json
import math
import time

import numpy as np

from . import power, rules, sim

N = 6
W_GRID = [10, 15, 20, 25, 30, 40, 50, 60, 80]
TARGET = 0.9
CONFIRMATORY_BUDGET = 38_000_000
CALIBRATION_BUDGET = 5_000_000
RESERVE_BUDGET = 5_000_000
REQUIRED = {"Agave": ("agave", 2), "JitoAgave": ("agave", 2), "Firedancer": ("fd2", 2)}
BONUS = {"HarmonicAgave": ("agave", 1), "HarmonicFiredancer": ("fd2", 1), "BamAgave": ("agave", 1),
         "BamFiredancer": ("fd2", 1), "Rakurai": ("agave", 1)}
REQUIRED_ARMS = ["P", "X", "R", "C", "F"]
BONUS_ARMS = ["P", "X", "C"]
# key rule per (family, arm); Arm F: a tie determinant (Firedancer's LIFO) as the effect
KEY_RULE = {
    ("agave", "P"): "R_RATIO_AGAVE", ("agave", "R"): "R_RATIO_AGAVE", ("agave", "C"): "R_RATIO_AGAVE",
    ("agave", "X"): "R_FIFO", ("agave", "F"): "TIE_ARRIVAL_LIFO",
    ("fd2", "P"): "R_RATIO_FD", ("fd2", "R"): "R_RATIO_FD", ("fd2", "C"): "R_RATIO_FD",
    ("fd2", "X"): "R_FIFO", ("fd2", "F"): "TIE_ARRIVAL_LIFO",
}


def model(fam: str, arm: str, rule: str) -> sim.Scheduler:
    if arm == "F":
        # A tie-break determinant exists (LIFO by arrival), with this family's interleaving.
        return sim.FD2 if fam == "fd2" else sim.Scheduler("fd", workers=4, batch_max=64)
    return power.model_for(fam, rule)


def power_curves(reps: int, nz: sim.Nuisance, rng: np.random.Generator, log=print) -> dict:
    out = {}
    for (fam, arm), rule in KEY_RULE.items():
        names = rules.RULES_BY_ARM[arm]
        m = model(fam, arm, rule)
        full = power.pool(m, arm, N, nz, reps, max(W_GRID), rng)
        half = power.pool(sim.Scheduler("mix", base_sched=m, p_rule=0.5), arm, N, nz, reps, max(W_GRID), rng)
        cur = {}
        for w in W_GRID:
            def pw(pl):
                return float(np.mean([power.holm_normal(x[:w], names).get(rule, 1.0) < power.ALPHA for x in pl]))
            cur[str(w)] = {"exact": pw(full), "half": pw(half)}
        out[f"{fam}/{arm}"] = {"rule": rule, "power": cur}
        log(f"{fam}/{arm} done")
    return out


def choose_w(curve: dict, arm: str) -> tuple[int, bool]:
    for w in W_GRID:
        if curve[str(w)]["half"] >= TARGET:
            break
    else:
        w = W_GRID[-1]
    reached = curve[str(w)]["half"] >= TARGET
    if arm == "X":
        w = int(math.ceil(w / 5) * 5)
    return w, reached


def interp_power(curve: dict, w: int, field: str) -> float:
    ws = [int(k) for k in curve]
    lo = max((x for x in ws if x <= w), default=min(ws))
    return curve[str(lo)][field]


def allocate(curves: dict, nz: sim.Nuisance) -> dict:
    spend = power.spend_per_window(N)
    urate = power.usable_rate(N, nz.p_land)
    rows = []

    def row(group, fam, n_val, arm, tier, w, reached, trimmed=False):
        c = curves[f"{fam}/{arm}"]
        attempted = math.ceil(w / urate)
        return {
                "group": group, "tier": tier, "family": fam, "validators": n_val, "arm": arm,
                "key_rule": c["rule"], "W_usable_per_validator": w, "attempted_per_validator": attempted,
                "expected_usable_total": round(w * n_val, 1), "attempted_total": attempted * n_val,
                "cost_lamports": round(attempted * n_val * spend[arm]),
                "power_exact": interp_power(c["power"], w, "exact"),
                "power_half": interp_power(c["power"], w, "half"),
                "power_target_reached": reached and not trimmed, "trimmed": trimmed,
            }

    def add(group, fam, n_val, arms, tier):
        for arm in arms:
            w, reached = choose_w(curves[f"{fam}/{arm}"]["power"], arm)
            rows.append(row(group, fam, n_val, arm, tier, w, reached))

    for g, (fam, k) in REQUIRED.items():
        add(g, fam, k, REQUIRED_ARMS, "required")
    for g, (fam, k) in BONUS.items():
        add(g, fam, k, BONUS_ARMS, "bonus")
    total = sum(r["cost_lamports"] for r in rows)
    f_bonus = []
    for g, (fam, k) in BONUS.items():
        c = curves[f"{fam}/F"]
        w, _ = choose_w(c["power"], "F")
        cost = math.ceil(w / urate) * k * spend["F"]
        f_bonus.append((g, fam, k, cost))
    if total + sum(x[3] for x in f_bonus) <= CONFIRMATORY_BUDGET:
        for g, fam, k, _ in f_bonus:
            add(g, fam, k, ["F"], "bonus")
        total = sum(r["cost_lamports"] for r in rows)
        bonus_f = True
    else:
        bonus_f = False
    while total > CONFIRMATORY_BUDGET:
        cand = [i for i, r in enumerate(rows) if r["tier"] == "bonus" and r["W_usable_per_validator"] > W_GRID[0]]
        if not cand:
            break
        i = max(cand, key=lambda i: rows[i]["cost_lamports"])
        r = rows[i]
        step = 5 if r["arm"] == "X" else None
        lower = [w for w in W_GRID if w < r["W_usable_per_validator"]]
        new_w = max(lower) if step is None else max(w for w in range(10, r["W_usable_per_validator"], 5))
        rows[i] = row(r["group"], r["family"], r["validators"], r["arm"], r["tier"], new_w, False, True)
        total = sum(x["cost_lamports"] for x in rows)
    quota: dict[str, dict[str, int]] = {}
    for r in rows:
        quota.setdefault(r["group"], {})[r["arm"]] = r["attempted_per_validator"]
    return {
        # read by the sender (`--allocation`): stratum -> arm -> attempted windows per validator
        "plan_quota": quota,
        "n": N, "usable_rate": urate, "spend_per_window_lamports": spend, "rows": rows,
        "confirmatory_total_lamports": total, "confirmatory_budget_lamports": CONFIRMATORY_BUDGET,
        "within_budget": total <= CONFIRMATORY_BUDGET, "bonus_arm_f_included": bonus_f,
        "calibration_budget_lamports": CALIBRATION_BUDGET, "reserve_budget_lamports": RESERVE_BUDGET,
        "grand_total_cap_lamports": CONFIRMATORY_BUDGET + CALIBRATION_BUDGET + RESERVE_BUDGET,
    }


def agave_c_control(reps: int, rng: np.random.Generator, w: int) -> dict:
    """Positive control: 95% predictive interval of the mean raw tau of R_RATIO_AGAVE in
    Arm C (vanilla Agave), at W usable windows, per nuisance level and their union."""
    pc = {}
    for lvl, nz in sim.NUISANCE_GRID.items():
        pl = power.pool(sim.AGAVE, "C", N, nz, reps, w, rng)
        means = [np.nanmean([x["R_RATIO_AGAVE"][0] for x in wins]) for wins in pl]
        pc[lvl] = [float(np.quantile(means, 0.025)), float(np.quantile(means, 0.975))]
    pc["union"] = [min(v[0] for v in pc.values()), max(v[1] for v in pc.values())]
    return pc


def run(seed: int, reps: int, nz: sim.Nuisance = sim.MEDIUM, log=print) -> dict:
    t0 = time.time()
    rng = np.random.default_rng(seed)
    curves = power_curves(reps, nz, rng, log)
    alloc = allocate(curves, nz)
    w_c = next(r["W_usable_per_validator"] for r in alloc["rows"] if r["group"] == "Agave" and r["arm"] == "C")
    return {"label": "SIMULATION", "seed": seed, "reps": reps, "nuisance": nz.__dict__, "w_grid": W_GRID,
            "target_half_power": TARGET, "curves": curves, "allocation": alloc,
            "positive_control_agave_C": {"W": w_c, "interval": agave_c_control(reps, rng, w_c)},
            "elapsed_s": round(time.time() - t0, 1)}


def render(res: dict) -> str:
    a = res["allocation"]
    out = ["## Power-balanced allocation (fills PREREGISTRATION §8)", "",
           f"> SIMULATION, N = {a['n']}, seed {res['seed']}, {res['reps']} replications, medium nuisance.",
           f"> Usable rate P(≥ 5 of 6 land) = {a['usable_rate']:.3f}. W = smallest W with half-strength",
           f"> power ≥ {res['target_half_power']} (grid {res['w_grid']}). X rounded up to a multiple of 5.", "",
           "| group | tier | validators | arm | key rule | W usable / validator | attempted / validator | expected usable (total) | cost (lamports) | power exact | power half | target reached |",
           "|---|---|---|---|---|---|---|---|---|---|---|---|"]
    for r in a["rows"]:
        out.append(f"| {r['group']} | {r['tier']} | {r['validators']} | {r['arm']} | {r['key_rule']} | "
                   f"{r['W_usable_per_validator']} | {r['attempted_per_validator']} | {r['expected_usable_total']:.0f} | "
                   f"{r['cost_lamports']:,} | {r['power_exact']:.2f} | {r['power_half']:.2f} | "
                   f"{'yes' if r['power_target_reached'] else ('**no (trimmed)**' if r.get('trimmed') else '**no**')} |")
    out += ["", f"- Confirmatory total: **{a['confirmatory_total_lamports']:,} lamports** "
                f"(budget {a['confirmatory_budget_lamports']:,}; within budget: {a['within_budget']}).",
            f"- Arm F for bonus groups included: {a['bonus_arm_f_included']}.",
            f"- Calibration ≤ {a['calibration_budget_lamports']:,}; funding/sweep/retry reserve ≤ "
            f"{a['reserve_budget_lamports']:,}; total cap {a['grand_total_cap_lamports']:,} (hard ceiling 50,000,000).",
            "", "### Power curves (half-strength rule; N = 6)", "",
            "| family/arm | rule | " + " | ".join(f"W={w}" for w in res["w_grid"]) + " |",
            "|---|---|" + "---|" * len(res["w_grid"])]
    for k, c in res["curves"].items():
        out.append(f"| {k} | {c['rule']} | " + " | ".join(f"{c['power'][str(w)]['half']:.2f}" for w in res["w_grid"]) + " |")
    pc = res["positive_control_agave_C"]
    out += ["", f"### Positive control: vanilla Agave, Arm C, R_RATIO_AGAVE, W = {pc['W']}", "",
            "95% predictive interval of the mean raw tau: " + ", ".join(
                f"{lvl} [{v[0]:.2f}, {v[1]:.2f}]" for lvl, v in pc["interval"].items() if lvl != "union"),
            f"; **registered (union): [{pc['interval']['union'][0]:.2f}, {pc['interval']['union'][1]:.2f}]**.", ""]
    return "\n".join(out)


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--seed", type=int, default=20261022)
    ap.add_argument("--reps", type=int, default=200)
    ap.add_argument("--out", required=True)
    ap.add_argument("--md")
    a = ap.parse_args()
    res = run(a.seed, a.reps, log=lambda m: print(m, flush=True))
    with open(a.out, "w", encoding="utf-8") as f:
        json.dump(res, f, indent=1, sort_keys=True)
    if a.md:
        with open(a.md, "w", encoding="utf-8") as f:
            f.write(render(res))
    print(f"SIMULATION written to {a.out} in {res['elapsed_s']} s; within budget: {res['allocation']['within_budget']}")


if __name__ == "__main__":
    main()
