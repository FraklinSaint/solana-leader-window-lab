"""README.md: every in-page anchor and every relative link resolves (the README is the judges' entry point)."""
import pathlib
import re

ROOT = pathlib.Path(__file__).resolve().parents[2]


def slug(heading: str) -> str:
    """GitHub's heading anchor: lower case, backticks and punctuation dropped, spaces to hyphens."""
    h = heading.strip().lower().replace("`", "")
    h = re.sub(r"[^\w\- ]", "", h, flags=re.U)
    return h.replace(" ", "-")


def test_slug_matches_github_for_the_headings_we_use():
    assert slug("What went wrong, and what this does not show") == "what-went-wrong-and-what-this-does-not-show"
    assert slug("Registration order: what a third party can check") == "registration-order-what-a-third-party-can-check"
    assert slug("Bounty considerations (one subsection each)") == "bounty-considerations-one-subsection-each"


def test_every_anchor_and_relative_link_in_the_readme_resolves():
    text = (ROOT / "README.md").read_text(encoding="utf-8")
    heads = {slug(m.group(2)) for m in re.finditer(r"^(#{1,6})\s+(.*)$", text, re.M)}
    bad_anchors = sorted(a for a in re.findall(r"\]\(#([^)]+)\)", text) if a not in heads)
    assert not bad_anchors, bad_anchors
    missing = sorted(l for l in set(re.findall(r"\]\(([^)#\s]+)(?:#[^)]*)?\)", text))
                     if not l.startswith(("http://", "https://")) and not (ROOT / l).exists())
    assert not missing, missing
