#!/usr/bin/env python3
"""Check every dependency's licence against the allow list in deny.toml, and
write the list of third-party packages that has to ship with the application.

    python3 packaging/licenses.py --check
    python3 packaging/licenses.py --report target/dist/THIRD-PARTY-PACKAGES.txt

cargo-deny does the authoritative check in CI. This script needs nothing but
Python and cargo, so it also runs on a developer machine, and it produces the
package list, which cargo-deny does not.

A licence expression is accepted when a way to satisfy it uses only allowed
licences: for `A OR B` one side is enough, for `A AND B` both are needed.
"""

import argparse
import json
import re
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent


def string_array(text, key):
    """The strings in the TOML array `key = [...]`. Enough for deny.toml, and it
    keeps this script running on Python versions that have no TOML parser."""
    body = re.search(rf"^{key}\s*=\s*\[(.*?)^\]", text, flags=re.S | re.M).group(1)
    body = re.sub(r"#.*", "", body)
    return re.findall(r'"([^"]*)"', body)


def allowed_licences():
    text = (ROOT / "deny.toml").read_text(encoding="utf-8")
    return set(string_array(text, "allow")), string_array(text, "targets")


def tokenize(expression):
    # Older crates write "MIT/Apache-2.0" for "MIT OR Apache-2.0".
    expression = expression.replace("/", " OR ")
    tokens = re.findall(r"\(|\)|[^\s()]+", expression)
    merged = []
    for token in tokens:
        if merged and merged[-1].endswith(" WITH"):
            merged[-1] += " " + token
        elif token.upper() == "WITH" and merged:
            merged[-1] += " WITH"
        else:
            merged.append(token)
    return merged


def accepted(expression, allow):
    """Whether `expression` can be satisfied with licences from `allow`."""
    tokens = tokenize(expression)
    position = 0

    def atom():
        nonlocal position
        token = tokens[position]
        position += 1
        if token == "(":
            value = either()
            position += 1  # the closing parenthesis
            return value
        return token in allow

    def both():
        nonlocal position
        value = atom()
        while position < len(tokens) and tokens[position].upper() == "AND":
            position += 1
            value = atom() and value
        return value

    def either():
        nonlocal position
        value = both()
        while position < len(tokens) and tokens[position].upper() == "OR":
            position += 1
            value = both() or value
        return value

    return either()


def packages(targets):
    """Third-party packages that are built for any of `targets`."""
    found = {}
    for target in targets:
        raw = subprocess.run(
            ["cargo", "metadata", "--format-version", "1", "--locked", "--filter-platform", target],
            cwd=ROOT, check=True, capture_output=True, text=True,
        ).stdout
        metadata = json.loads(raw)
        ours = set(metadata["workspace_members"])
        # `packages` lists everything in the lock file; `resolve` is what this
        # target actually uses.
        used = {node["id"] for node in metadata["resolve"]["nodes"]}
        for package in metadata["packages"]:
            if package["id"] in ours or package["id"] not in used:
                continue
            found[(package["name"], package["version"])] = package
    return [found[key] for key in sorted(found)]


# Data that packages build into the binary under licences of its own, beside
# the package's code licence. Their notices are copied into the report in
# full: the BSD licences ask for that in binary distributions.
EMBEDDED_DATA = [
    {
        "package": "hayro-font",
        "what": "The 14 standard PDF fonts (Foxit's, from PDFium)",
        "license": "BSD-3-Clause",
        "notice": "assets/LICENSE_FOXIT",
    },
    {
        "package": "hayro-cmap",
        "what": "Adobe's predefined CMaps for Chinese, Japanese and Korean text",
        "license": "BSD-3-Clause",
        "notice": "assets/LICENSE.txt",
    },
    {
        "package": "hayro-interpret",
        "what": "Two ICC colour profiles (CMYK and Lab)",
        "license": "CC0-1.0",
        "notice": "assets/README.md",
    },
]


def embedded_data(listed, allow):
    """The embedded data of packages in the build: what it is, its licence,
    the notice text, and problems (licence not allowed, notice not found)."""
    found, problems = [], []
    by_name = {}
    for package in listed:
        by_name.setdefault(package["name"], package)
    for entry in EMBEDDED_DATA:
        package = by_name.get(entry["package"])
        if package is None:
            continue
        if not accepted(entry["license"], allow):
            problems.append(f'{entry["package"]}: {entry["what"]}: {entry["license"]}')
        notice = Path(package["manifest_path"]).parent / entry["notice"]
        try:
            text = notice.read_text(encoding="utf-8")
        except OSError:
            problems.append(f'{entry["package"]}: notice {entry["notice"]} not found')
            text = ""
        found.append((package, entry, text))
    return found, problems


def main():
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("--check", action="store_true", help="fail if a licence is not allowed")
    parser.add_argument("--report", metavar="FILE", help="write the package list to FILE")
    args = parser.parse_args()

    allow, targets = allowed_licences()
    listed = packages(targets)
    refused = [
        package for package in listed
        if not package.get("license") or not accepted(package["license"], allow)
    ]
    data, data_problems = embedded_data(listed, allow)

    if args.report:
        lines = [
            "Third-party packages built into this application.",
            "Each is used under the licence named beside it; where a package offers",
            "a choice, the first licence that this project's policy allows applies.",
            "",
        ]
        for package in listed:
            source = package.get("repository") or "https://crates.io/crates/" + package["name"]
            lines.append(f'{package["name"]} {package["version"]}  |  {package.get("license") or "UNKNOWN"}  |  {source}')
        if data:
            lines += ["", "", "Data built in by these packages, under licences of its own.", ""]
        for package, entry, text in data:
            lines += [
                "=" * 72,
                f'{entry["what"]}, in {package["name"]} {package["version"]}  |  {entry["license"]}',
                "=" * 72,
                text.rstrip(),
                "",
            ]
        Path(args.report).parent.mkdir(parents=True, exist_ok=True)
        Path(args.report).write_text("\n".join(lines) + "\n", encoding="utf-8")

    print(f"{len(listed)} third-party packages across {len(targets)} targets")
    for package in refused:
        print(f'  NOT ALLOWED: {package["name"]} {package["version"]}: {package.get("license") or "no licence declared"}')
    print(f"{len(data)} packages with data under licences of their own")
    for problem in data_problems:
        print(f"  EMBEDDED DATA: {problem}")
    if args.check and (refused or data_problems):
        sys.exit(1)
    if not refused and not data_problems:
        print("every licence is on the allow list")


if __name__ == "__main__":
    main()
