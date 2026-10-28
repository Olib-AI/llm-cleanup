#!/usr/bin/env python3
"""Ensure the app's displayed and bundled versions match the Cargo workspace."""

from pathlib import Path
import plistlib
import re
import sys
import tomllib

ROOT = Path(__file__).resolve().parent.parent


def main() -> int:
    with (ROOT / "Cargo.toml").open("rb") as handle:
        version = tomllib.load(handle)["workspace"]["package"]["version"]
    with (ROOT / "packaging" / "Info.plist").open("rb") as handle:
        plist = plistlib.load(handle)
    slint = (ROOT / "crates" / "cleanup-gui" / "ui" / "appwindow.slint").read_text(encoding="utf-8")
    match = re.search(r'in property <string> app-version:\s*"([^"]+)";', slint)
    actual = {
        "CFBundleVersion": plist.get("CFBundleVersion"),
        "CFBundleShortVersionString": plist.get("CFBundleShortVersionString"),
        "Slint app-version default": match.group(1) if match else None,
    }
    mismatches = [f"{name}: {value!r} (expected {version})" for name, value in actual.items() if value != version]
    if mismatches:
        print("Version mismatch:\n" + "\n".join(mismatches), file=sys.stderr)
        return 1
    print(f"Workspace, macOS bundle, and GUI version agree: {version}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
