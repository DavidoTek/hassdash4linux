#!/usr/bin/env python3
"""Generate cargo-sources.json (offline crate sources for flatpak-builder)
from Cargo.lock. Only crates.io dependencies are supported; that is all
hassdash4linux uses. No third-party Python modules needed.

Usage: build-aux/flatpak/gen-cargo-sources.py [Cargo.lock] [output.json]
"""
import json
import sys
import tomllib
from pathlib import Path

root = Path(__file__).resolve().parents[2]
lock_path = Path(sys.argv[1]) if len(sys.argv) > 1 else root / "Cargo.lock"
out_path = Path(sys.argv[2]) if len(sys.argv) > 2 else Path(__file__).with_name("cargo-sources.json")

lock = tomllib.loads(lock_path.read_text())
sources = []
for pkg in lock["package"]:
    src = pkg.get("source")
    if src is None:
        continue  # workspace member
    if src != "registry+https://github.com/rust-lang/crates.io-index":
        sys.exit(f"unsupported source for {pkg['name']}: {src}")
    name, version, checksum = pkg["name"], pkg["version"], pkg["checksum"]
    dest = f"cargo/vendor/{name}-{version}"
    sources.append({
        "type": "archive",
        "archive-type": "tar-gzip",
        "url": f"https://static.crates.io/crates/{name}/{name}-{version}.crate",
        "sha256": checksum,
        "dest": dest,
    })
    sources.append({
        "type": "inline",
        "contents": json.dumps({"package": checksum, "files": {}}),
        "dest": dest,
        "dest-filename": ".cargo-checksum.json",
    })
sources.append({
    "type": "inline",
    "contents": '[source.vendored-sources]\ndirectory = "cargo/vendor"\n\n'
                '[source.crates-io]\nreplace-with = "vendored-sources"\n',
    "dest": "cargo",
    "dest-filename": "config.toml",
})
out_path.write_text(json.dumps(sources, indent=4) + "\n")
print(f"wrote {out_path} ({(len(sources) - 1) // 2} crates)")
