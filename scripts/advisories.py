#!/usr/bin/env python3
"""Every advisory .cargo/audit.toml ignores has an owner, a reason and a review date that has not
passed, in docs/advisories.yaml. Nothing is ignored by accident, or for ever."""

import datetime
import re
import sys
from pathlib import Path

import yaml

ROOT = Path(__file__).resolve().parent.parent


def problems() -> list:
    ignored = set(re.findall(r'"(RUSTSEC-[\d-]+)"', (ROOT / ".cargo" / "audit.toml").read_text()))
    entries = yaml.safe_load((ROOT / "docs" / "advisories.yaml").read_text())["advisories"]
    bad = [f"{i}: ignored in .cargo/audit.toml with no entry in docs/advisories.yaml" for i in ignored - {a["id"] for a in entries}]
    bad += [f"{a['id']}: no longer ignored; remove its entry" for a in entries if a["id"] not in ignored]
    today = datetime.date.today()
    for a in entries:
        missing = [k for k in ("owner", "reason", "applicability", "review_by", "resolved_when") if not a.get(k)]
        if missing:
            bad.append(f"{a['id']}: lacks {', '.join(missing)}")
        elif a["review_by"] < today:
            bad.append(f"{a['id']}: its review was due on {a['review_by']}")
    return bad


if __name__ == "__main__":
    found = problems()
    print("\n".join(found) or "every accepted advisory has an owner and a review date")
    sys.exit(1 if found else 0)
