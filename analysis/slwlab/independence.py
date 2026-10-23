"""Independence units: chosen validators grouped by the IP of their gossip TPU address.

Several validators can advertise one TPU IP (gossip ``tpuQuic``). Our transactions to them then
pass through the same address, so their results are not independent observations of
independent schedulers. Rule (PREREGISTRATION §6c): verdicts stay per validator, but every
statement that counts or pools validators counts **units**, and a unit is the resampling unit
for any cross-validator interval. What runs behind a shared address is not inferred.

``python -m slwlab.independence`` writes ``data/snapshots/shared_tpu_ips_<ts>.json`` (DERIVED from
the selection and its cluster_nodes snapshot) for selections made before ``sender
select-validators`` recorded ``shared_tpu_ips`` itself.
"""

from __future__ import annotations

import argparse
import gzip
import json
import pathlib
from collections import defaultdict


def tpu_ip(addr: str | None) -> str | None:
    """'198.13.140.105:12341' -> '198.13.140.105'; '[2001:db8::1]:8009' -> '2001:db8::1'."""
    if not addr:
        return None
    host = addr.rsplit(":", 1)[0] if ":" in addr else addr
    return host.strip("[]")


def shared_tpu_ips(cluster_nodes: list[dict], picked: list[dict]) -> list[dict]:
    """Same output as select.rs ``shared_tpu_ips``: picked validators whose TPU IP is shared."""
    nodes_on: dict[str, int] = defaultdict(int)
    for n in cluster_nodes:
        ip = tpu_ip(n.get("tpuQuic"))
        if ip:
            nodes_on[ip] += 1
    by_ip: dict[str, list[str]] = defaultdict(list)
    for p in picked:
        ip = tpu_ip(p.get("tpu_quic"))
        if ip:
            by_ip[ip].append(p["identity"])
    return [{"ip": ip, "gossip_nodes_on_ip": nodes_on.get(ip, 0), "picked_on_ip": sorted(ids)}
            for ip, ids in sorted(by_ip.items()) if nodes_on.get(ip, 0) > 1 or len(ids) > 1]


def units(picked: list[dict]) -> dict[str, str]:
    """identity -> unit id (the TPU IP; a validator without an address is its own unit)."""
    return {p["identity"]: tpu_ip(p.get("tpu_quic")) or f"no-tpu:{p['identity']}" for p in picked}


def units_from_manifest(lines: list[dict]) -> dict[str, str]:
    """identity -> unit id from manifest rows (``tx.tpu_quic``, recorded at plan time)."""
    out: dict[str, str] = {}
    for l in lines:
        t = l["tx"]
        out.setdefault(t["identity"], tpu_ip(t.get("tpu_quic")) or f"no-tpu:{t['identity']}")
    return out


def derive(snapshots: pathlib.Path, selection: pathlib.Path) -> dict:
    sel = json.loads(selection.read_text(encoding="utf-8"))
    with gzip.open(snapshots / sel["raw"]["cluster_nodes"], "rt", encoding="utf-8") as f:
        nodes = json.load(f)
    shared = shared_tpu_ips(nodes, sel["picked"])
    u = units(sel["picked"])
    return {
        "label": "DERIVED",
        "from": [selection.name, sel["raw"]["cluster_nodes"]],
        "note": "Picked validators whose gossip tpuQuic IP is advertised by more than one node, or "
                "shared by more than one picked validator. What runs at that address is not inferred.",
        "shared_tpu_ips": shared,
        "picked": len(sel["picked"]),
        "independence_units": len(set(u.values())),
    }


def main(argv: list[str] | None = None) -> None:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--snapshots", default="../data/snapshots")
    ap.add_argument("--selection", help="default: newest selection_*.json")
    a = ap.parse_args(argv)
    snaps = pathlib.Path(a.snapshots)
    sel = pathlib.Path(a.selection) if a.selection else sorted(snaps.glob("selection_*.json"))[-1]
    d = derive(snaps, sel)
    out = snaps / f"shared_tpu_ips_{sel.stem.split('_', 1)[1]}.json"
    out.write_text(json.dumps(d, indent=1, sort_keys=True) + "\n", encoding="utf-8", newline="\n")
    print(f"{d['picked']} picked validators, {d['independence_units']} independence units -> {out}")


if __name__ == "__main__":
    main()
