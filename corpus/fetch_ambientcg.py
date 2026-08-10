#!/usr/bin/env python3
"""Download ambientCG 1K-PNG materials into corpus/raw/ and refresh manifest entries."""

from __future__ import annotations

import json
import sys
import urllib.request
import zipfile
from pathlib import Path

ROOT = Path(__file__).resolve().parent
RAW = ROOT / "raw"
MANIFEST_PATH = ROOT / "manifest.json"
USER_AGENT = "rusty_dds-corpus-fetch/0.1 (+https://github.com/Remade-With-Rust/rusty_dds)"

# Keep only cook-relevant maps (skip Displacement, Metalness, etc. for size).
KEEP_SUFFIXES = ("Color", "NormalGL", "Roughness")


def download(url: str, dest: Path) -> None:
    dest.parent.mkdir(parents=True, exist_ok=True)
    req = urllib.request.Request(url, headers={"User-Agent": USER_AGENT})
    with urllib.request.urlopen(req, timeout=120) as resp, open(dest, "wb") as out:
        while True:
            chunk = resp.read(1 << 20)
            if not chunk:
                break
            out.write(chunk)


def extract_maps(zip_path: Path, asset_id: str, out_dir: Path) -> list[Path]:
    out_dir.mkdir(parents=True, exist_ok=True)
    written: list[Path] = []
    with zipfile.ZipFile(zip_path, "r") as zf:
        for info in zf.infolist():
            if info.is_dir():
                continue
            name = Path(info.filename).name
            if not name.lower().endswith(".png"):
                continue
            stem = Path(name).stem
            if not any(stem.endswith(f"_{s}") for s in KEEP_SUFFIXES):
                continue
            target = out_dir / name
            if not target.exists():
                with zf.open(info) as src, open(target, "wb") as dst:
                    dst.write(src.read())
            written.append(target)
    if not written:
        raise RuntimeError(f"no Color/NormalGL/Roughness PNGs in {zip_path.name} ({asset_id})")
    return sorted(written)


def classify(path: Path) -> tuple[str, str] | None:
    stem = path.stem
    for suffix, role in (
        ("Color", "albedo"),
        ("NormalGL", "normal"),
        ("Roughness", "mask"),
    ):
        if stem.endswith(f"_{suffix}") or stem.endswith(f"-{suffix}"):
            return role, suffix
    return None


def targets_for(role: str) -> list[str]:
    return {
        "albedo": ["bc1", "bc7"],
        "normal": ["bc5u", "bc5s"],
        "mask": ["bc4u", "bc4s"],
    }[role]


def main() -> int:
    man = json.loads(MANIFEST_PATH.read_text(encoding="utf-8"))
    RAW.mkdir(parents=True, exist_ok=True)
    zips = RAW / "_zips"
    zips.mkdir(parents=True, exist_ok=True)

    entries: list[dict] = []
    for asset in man["assets"]:
        aid = asset["id"]
        zip_name = f"{aid}_1K-PNG.zip"
        zip_path = zips / zip_name
        url = asset["download"]
        if not zip_path.exists() or zip_path.stat().st_size < 1000:
            print(f"download {aid} …")
            download(url, zip_path)
            print(f"  -> {zip_path.stat().st_size} bytes")
        else:
            print(f"skip download {aid} (zip present)")

        asset_dir = RAW / aid
        paths = extract_maps(zip_path, aid, asset_dir)
        print(f"  maps: {', '.join(p.name for p in paths)}")

        for p in paths:
            kind = classify(p)
            if kind is None:
                continue
            role, suffix = kind
            rel = p.relative_to(ROOT).as_posix()
            entries.append(
                {
                    "id": f"{aid}_{suffix}",
                    "asset": aid,
                    "role": role,
                    "map": suffix,
                    "path": rel,
                    "targets": targets_for(role),
                    "source_url": asset["source_url"],
                    "license": man["license"],
                }
            )

    man["entries"] = entries
    MANIFEST_PATH.write_text(json.dumps(man, indent=2) + "\n", encoding="utf-8")
    print(f"wrote {len(entries)} entries -> {MANIFEST_PATH}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
