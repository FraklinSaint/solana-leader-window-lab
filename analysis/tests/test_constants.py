import pathlib
import re

from slwlab import ALPENGLOW_GENESIS_SLOT

GUARD_RS = pathlib.Path(__file__).resolve().parents[2] / "crates" / "common" / "src" / "guard.rs"


def test_alpenglow_slot_matches_rust_guard():
    m = re.search(r"pub const ALPENGLOW_GENESIS_SLOT: u64 = ([\d_]+);", GUARD_RS.read_text())
    assert m, "constant not found in guard.rs"
    assert int(m.group(1).replace("_", "")) == ALPENGLOW_GENESIS_SLOT
