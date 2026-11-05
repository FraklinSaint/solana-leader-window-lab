"""README.md and docs/README_FULL.md: every in-page anchor and every relative link resolves (the README is the judges' entry point)."""
import pathlib
import re

import pytest

ROOT = pathlib.Path(__file__).resolve().parents[2]
FILES = [ROOT / "README.md", ROOT / "docs" / "README_FULL.md"]


def slug(heading: str) -> str:
    """GitHub's heading anchor: lower case, backticks and punctuation dropped, spaces to hyphens."""
    h = heading.strip().lower().replace("`", "")
    h = re.sub(r"[^\w\- ]", "", h, flags=re.U)
    return h.replace(" ", "-")


def anchors(path: pathlib.Path) -> set:
    text = path.read_text(encoding="utf-8")
    return {slug(m.group(2)) for m in re.finditer(r"^(#{1,6})\s+(.*)$", text, re.M)}


def test_slug_matches_github_for_the_headings_we_use():
    assert slug("What went wrong, and what this does not show") == "what-went-wrong-and-what-this-does-not-show"
    assert slug("Registration order: what a third party can check") == "registration-order-what-a-third-party-can-check"
    assert slug("Bounty considerations (one subsection each)") == "bounty-considerations-one-subsection-each"


@pytest.mark.parametrize("path", FILES, ids=lambda p: p.relative_to(ROOT).as_posix())
def test_every_anchor_and_relative_link_resolves(path):
    text = path.read_text(encoding="utf-8")
    bad_anchors = sorted(a for a in re.findall(r"\]\(#([^)]+)\)", text) if a not in anchors(path))
    assert not bad_anchors, bad_anchors
    missing, bad_cross = [], []
    for target, frag in re.findall(r"\]\(([^)#\s]+)(?:#([^)]*))?\)", text):
        if target.startswith(("http://", "https://")):
            continue
        dest = (path.parent / target).resolve()
        if not dest.exists():
            missing.append(target)
        elif frag and dest.suffix == ".md" and frag not in anchors(dest):
            bad_cross.append(f"{target}#{frag}")
    assert not missing, sorted(set(missing))
    assert not bad_cross, sorted(set(bad_cross))


def test_short_readme_stays_short_and_keeps_cited_headings():
    short = (ROOT / "README.md").read_text(encoding="utf-8")
    assert len(short.splitlines()) <= 150
    # the registration (PREREGISTRATION.md, tagged) cites these README sections by name
    assert {"limitations-wi-fi-no-ethernet", "what-this-does-not-separate"} <= anchors(ROOT / "README.md")
    assert {"limitations-wi-fi-no-ethernet", "what-this-does-not-separate", "running-natively", "payer-sizing", "verify-no-trust-in-our-collector"} <= anchors(ROOT / "docs" / "README_FULL.md")
