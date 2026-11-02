#!/usr/bin/env python3
"""What a release is and what it passed, for attaching to it.

    scripts/qualification.py <folder of artifacts>     # JSON on stdout

Exact commits, the lockfile, the frontend, the features and each artifact's hash, with the run that
tested exactly those. A label it does not give is one nobody earned: see docs/compatibility.md.
"""

import hashlib
import json
import os
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent


def sha256(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def locked(lock: str, name: str) -> str | None:
    found = re.search(rf'name = "{name}"\nversion = "[^"]+"\nsource = "git\+[^#]+#([0-9a-f]+)"', lock)
    return found.group(1) if found else None


def main() -> int:
    folder = Path(sys.argv[1])
    lock = (ROOT / "Cargo.lock").read_text()
    gateway = (ROOT / "gateway" / "Cargo.toml").read_text()
    repository, run = os.environ.get("GITHUB_REPOSITORY", ""), os.environ.get("GITHUB_RUN_ID", "")
    report = {
        "version": re.search(r'^version = "(.+)"$', (ROOT / "Cargo.toml").read_text(), re.M).group(1),
        "commit": os.environ.get("GITHUB_SHA"),
        "lockfile_sha256": sha256(ROOT / "Cargo.lock"),
        "ferrum_commit": locked(lock, "ferrum-ops-contract"),
        "frontend_commit": (ROOT / "frontend.rev").read_text().strip(),
        "default_features": re.findall(r'"([^"]+)"', re.search(r"^default = \[(.*)\]$", gateway, re.M).group(1)),
        "artifacts": {p.name: sha256(p) for p in sorted(folder.iterdir()) if p.is_file() and not p.name.endswith((".sha256", ".json"))},
        "tested": {"suite": "ci.yml, on the lockfile and the frontend above, with nothing updated", "run": f"https://github.com/{repository}/actions/runs/{run}"},
        "labels": {"implemented_transport": "docs/compatibility.md", "independent_software_tested": "docs/compatibility.md", "vendor_simulator_tested": [], "physical_equipment_qualified": [], "production_profile_supported": []},
        "exclusions": "Nothing here has run on physical equipment. Offline, a gateway runs what it was given and originates nothing: docs/gateway.md.",
    }
    print(json.dumps(report, indent=2))
    return 0


if __name__ == "__main__":
    sys.exit(main())
