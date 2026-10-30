#!/usr/bin/env python3
"""Friend kit for operator B (AMENDMENT 3, platform B, exploratory). Stdlib only, reads no data/.

    python scripts/friend_kit_build.py --write-files   # allocation + selection into docs/FRIEND_KIT/files/
    python scripts/friend_kit_build.py --check         # files on disk == fresh build
    python scripts/friend_kit_build.py --zip OUT.zip --sender target/release/sender.exe [--master-keypair PATH]

The allocation file is generated here, never edited by hand: C, F, P at 4 and X at 5 attempted windows
for each of six validators (9eGrDohd, JUPiTERr, HEL1USMZ, E1r4Psq8, Fd7btgyS, DRpbCBMx), format of
alloc_calibrated_session4of4.json. The selection file is the amendment-1 selection copy reduced to those six
`picked` entries (public chain data, no candidates, no raw evidence). Nothing of ours goes into the zip: no
data/, no .env, no secrets/ except, only with --master-keypair, the throwaway master of operator B (and the
CLUSTER marker file, which holds the word mainnet). The zip itself is not committed; CHECKSUMS.txt is.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import pathlib
import sys
import zipfile

ROOT = pathlib.Path(__file__).resolve().parents[1]
KIT = ROOT / "docs" / "FRIEND_KIT"
FILES = KIT / "files"
SRC_SELECTION = ROOT / "analysis" / "sim" / "results" / "sessions" / "selection_amendment1_no_EvnRmnMr.json"
SRC_ALLOC = ROOT / "analysis" / "sim" / "results" / "sessions" / "alloc_calibrated_session4of4.json"
SENDER_SHA = "475fe47a880acab1504ea752633968dc7f1abe9abb09659d80edab663357e718"
VALIDATORS = ["9eGrDohd", "JUPiTERr", "HEL1USMZ", "E1r4Psq8", "Fd7btgyS", "DRpbCBMx"]
QUOTA = {"C": 4, "F": 4, "P": 4, "X": 5}  # attempted windows per validator and arm
ALLOC_NAME = "alloc_platform_b.json"
SELECTION_NAME = "selection_20261030T180000Z_platformB.json"
KIT_TEXT = ["README_FOR_OPERATOR_B.md", "COMMANDS_B.txt", "env.template"]
FORBIDDEN = ("data/", ".env", "secrets/payers", "sink.json", "id.json")


def dump(doc: dict) -> bytes:
    return (json.dumps(doc, indent=1, sort_keys=True) + "\n").encode("utf-8")


def build() -> dict[str, bytes]:
    sel = json.loads(SRC_SELECTION.read_text(encoding="utf-8"))
    picked = [p for p in sel["picked"] if p["identity"][:8] in VALIDATORS]
    if sorted(p["identity"][:8] for p in picked) != sorted(VALIDATORS):
        raise SystemExit("selection does not hold all six validators")
    strata = sorted({p["stratum"] for p in picked})
    out_sel = {"cluster": sel["cluster"], "epoch": sel["epoch"], "min_windows": sel["min_windows"], "slot": sel["slot"], "ts": sel["ts"],
               "notes": [f"Reduced copy of {SRC_SELECTION.name} for AMENDMENT 3 (platform B): only the six picked validators {VALIDATORS}; "
                         "no candidates, no raw evidence."],
               "picked": picked}
    q4 = json.loads(SRC_ALLOC.read_text(encoding="utf-8"))
    plan = {s: dict(QUOTA) for s in strata}
    spend = q4["allocation"]["spend_per_window_lamports"]
    alloc = {
        "label": "PLATFORM B ALLOCATION FILE (AMENDMENT 3, 2026-10-30; scripts/friend_kit_build.py; exploratory, never pooled)",
        "source": {"file": SRC_ALLOC.name, "selection": SELECTION_NAME},
        "session": "platform_b", "sessions": 1, "cumulative_share": "platform B only",
        "dropped_strata": [], "excluded_validators": [],
        "allocation": {"n": q4["allocation"]["n"], "usable_rate": q4["allocation"]["usable_rate"],
                       "spend_per_window_lamports": spend, "plan_quota": plan},
        "full_plan_quota": plan,
    }
    return {ALLOC_NAME: dump(alloc), SELECTION_NAME: dump(out_sel)}


def sha(b: bytes) -> str:
    return hashlib.sha256(b).hexdigest()


def kit_members(sender: pathlib.Path | None, master: pathlib.Path | None) -> dict[str, bytes]:
    gen = build()
    m: dict[str, bytes] = {}
    if sender is not None:
        b = sender.read_bytes()
        if sha(b) != SENDER_SHA:
            raise SystemExit(f"{sender} is not the approved sender (475fe47a880a): {sha(b)}")
        m["sender.exe"] = b
    m["allocation/" + ALLOC_NAME] = gen[ALLOC_NAME]
    m["data/snapshots/" + SELECTION_NAME] = gen[SELECTION_NAME]
    m["secrets/CLUSTER"] = b"mainnet"
    for name in KIT_TEXT:
        m[name] = (KIT / name).read_bytes()
    if master is not None:
        b = master.read_bytes()
        if not (b.startswith(b"[") and b.rstrip().endswith(b"]")):
            raise SystemExit("--master-keypair must be a Solana keypair JSON file (an array of 64 numbers)")
        m["secrets/master.json"] = b
    for name in m:
        if name.startswith(FORBIDDEN) and name not in ("secrets/master.json", "data/snapshots/" + SELECTION_NAME):
            raise SystemExit(f"forbidden member {name}")
    return m


def checksums(m: dict[str, bytes]) -> bytes:
    lines = [f"{sha(b)}  {name}" for name, b in sorted(m.items()) if name != "secrets/master.json"]
    return ("\n".join(lines) + "\n").encode("utf-8")


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    g = ap.add_mutually_exclusive_group(required=True)
    g.add_argument("--write-files", action="store_true")
    g.add_argument("--check", action="store_true")
    g.add_argument("--zip")
    ap.add_argument("--sender", default=str(ROOT / "target" / "release" / "sender.exe"))
    ap.add_argument("--master-keypair")
    a = ap.parse_args(argv)
    gen = build()
    if a.write_files or a.check:
        FILES.mkdir(parents=True, exist_ok=True)
        bad = 0
        want = dict(gen)
        for name, b in want.items():
            p = FILES / name
            if a.check:
                ok = p.exists() and p.read_bytes() == b
                print(("ok       " if ok else "DIFFERS  ") + str(p.relative_to(ROOT)))
                bad += 0 if ok else 1
            else:
                p.write_bytes(b)
                print(f"wrote {p.relative_to(ROOT)}  sha256 {sha(b)}")
        if a.check:
            # CHECKSUMS.txt lists the kit without sender.exe's bytes being in the repo: its hash is the approved one
            m = kit_members(None, None)
            text = checksums(m).decode() + f"{SENDER_SHA}  sender.exe\n"
            p = KIT / "CHECKSUMS.txt"
            ok = p.exists() and p.read_text(encoding="utf-8") == text
            print(("ok       " if ok else "DIFFERS  ") + str(p.relative_to(ROOT)))
            bad += 0 if ok else 1
        else:
            m = kit_members(None, None)
            (KIT / "CHECKSUMS.txt").write_text(checksums(m).decode() + f"{SENDER_SHA}  sender.exe\n", encoding="utf-8", newline="\n")
            print("wrote docs/FRIEND_KIT/CHECKSUMS.txt")
        return 1 if bad else 0
    m = kit_members(pathlib.Path(a.sender), pathlib.Path(a.master_keypair) if a.master_keypair else None)
    m["CHECKSUMS.txt"] = checksums(m)
    with zipfile.ZipFile(a.zip, "w", zipfile.ZIP_DEFLATED) as z:
        for name, b in sorted(m.items()):
            z.writestr(name, b)
    zb = pathlib.Path(a.zip).read_bytes()
    print(f"wrote {a.zip}: {len(m)} members, {len(zb)} bytes, sha256 {sha(zb)}; master keypair inside: {'secrets/master.json' in m}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
