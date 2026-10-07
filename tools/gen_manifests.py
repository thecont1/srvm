#!/usr/bin/env python3
"""Generate Scoop and winget manifests from a cargo-dist release manifest.

Every URL, version, and hash is derived from the dist manifest plus the
published `.sha256` sidecars — nothing is invented. The intended flow runs
this against the `dist-manifest.json` attached to the GitHub release (which
carries the real URLs), with the release's `.sha256` files unpacked next to
it.

Usage:
  tools/gen_manifests.py \
    --manifest dist-manifest.json \
    --sha256-dir ./checksums \
    --repo thecont1/srvm \
    --tag v0.1.0 \
    [--out-dir ./out]

Gate note: submitting to the actual Scoop bucket / winget-pkgs repository is
an approval-gated external step; this generator only produces the files and
validates their contents locally.
"""

import argparse
import json
import re
import sys
from pathlib import Path
from typing import NoReturn

WINDOWS_ARCH = {
    "x86_64-pc-windows-msvc": ("64bit", "x64"),
    "aarch64-pc-windows-msvc": ("arm64", "arm64"),
}


def fail(msg: str) -> NoReturn:
    print(f"error: {msg}", file=sys.stderr)
    sys.exit(1)


def load_manifest(path: Path) -> dict:
    with open(path) as f:
        return json.load(f)


def release_url(repo: str, tag: str, name: str) -> str:
    return f"https://github.com/{repo}/releases/download/{tag}/{name}"


def sha256_hex(sidecar: Path, filename: str) -> str:
    if not sidecar.exists():
        fail(
            f"checksum sidecar {sidecar} not found; run dist build or download "
            f"the release's .sha256 files before generating manifests"
        )
    text = sidecar.read_text()
    m = re.search(r"\b([0-9a-fA-F]{64})\b", text)
    if not m:
        fail(f"no sha256 hex found in {sidecar}")
    return m.group(1).lower()


def windows_archives(manifest: dict) -> dict:
    """{triple: artifact} for the windows executable-zip artifacts."""
    found = {}
    for name, art in manifest.get("artifacts", {}).items():
        if art.get("kind") != "executable-zip":
            continue
        for triple in art.get("target_triples", []):
            if triple in WINDOWS_ARCH:
                found[triple] = art
    missing = sorted(set(WINDOWS_ARCH) - set(found))
    if missing:
        fail(f"manifest lacks windows archives for: {missing}")
    return found


def scoop_manifest(manifest: dict, repo: str, tag: str, version: str, sha_dir: Path) -> dict:
    arts = windows_archives(manifest)
    arch = {}
    for triple, art in arts.items():
        scoop_arch, _ = WINDOWS_ARCH[triple]
        name = art["name"]
        hexhash = sha256_hex(sha_dir / f"{name}.sha256", name)
        arch[scoop_arch] = {
            "url": release_url(repo, tag, name),
            "hash": f"sha256:{hexhash}",
            "bin": "srvm.exe",
        }
    return {
        "version": version,
        "description": "Zero-config universal app launcher",
        "homepage": f"https://github.com/{repo}",
        "license": "MIT",
        "architecture": arch,
        "checkver": {"github": f"https://github.com/{repo}"},
        "autoupdate": {
            "architecture": {
                key: {
                    "url": release_url(repo, "v$version", arts[triple]["name"]),
                    "hash": {"url": f"$url.sha256"},
                }
                for triple, (key, _) in ((t, WINDOWS_ARCH[t]) for t in arts)
            }
        },
    }


def winget_manifests(manifest: dict, repo: str, tag: str, version: str, sha_dir: Path, identifier: str) -> dict:
    arts = windows_archives(manifest)
    installers = []
    for triple, art in arts.items():
        _, winget_arch = WINDOWS_ARCH[triple]
        name = art["name"]
        hexhash = sha256_hex(sha_dir / f"{name}.sha256", name)
        installers.append(
            {
                "Architecture": winget_arch,
                "InstallerUrl": release_url(repo, tag, name),
                "InstallerSha256": hexhash,
            }
        )
    return {
        f"{identifier}.installer.yaml": {
            "PackageIdentifier": identifier,
            "PackageVersion": version,
            "InstallerType": "zip",
            "NestedInstallerType": "portable",
            "NestedInstallerFiles": [{"RelativeFilePath": "srvm.exe"}],
            "Installers": installers,
            "ManifestType": "installer",
            "ManifestVersion": "1.6.0",
        },
        f"{identifier}.locale.en-US.yaml": {
            "PackageIdentifier": identifier,
            "PackageVersion": version,
            "PackageLocale": "en-US",
            "Publisher": "thecont1",
            "PackageName": "srvm",
            "ShortDescription": "Zero-config universal app launcher",
            "License": "MIT",
            "PackageUrl": f"https://github.com/{repo}",
            "ManifestType": "defaultLocale",
            "ManifestVersion": "1.6.0",
        },
        f"{identifier}.yaml": {
            "PackageIdentifier": identifier,
            "PackageVersion": version,
            "DefaultLocale": "en-US",
            "ManifestType": "version",
            "ManifestVersion": "1.6.0",
        },
    }


def emit_scalar(value) -> str:
    if isinstance(value, bool):
        return "true" if value else "false"
    if isinstance(value, str):
        return value
    return str(value)


def emit_yaml(value, indent: int = 0) -> list[str]:
    """Winget-style YAML lines: lists introduce items with '- ', nested keys
    align under the first key."""
    pad = "  " * indent
    if isinstance(value, dict):
        lines: list[str] = []
        for key, item in value.items():
            if isinstance(item, (dict, list)):
                lines.append(f"{pad}{key}:")
                lines.extend(emit_yaml(item, indent + 1))
            else:
                lines.append(f"{pad}{key}: {emit_scalar(item)}")
        return lines
    if isinstance(value, list):
        lines = []
        for item in value:
            if isinstance(item, dict):
                first = True
                for key, child in item.items():
                    prefix = f"{pad}- " if first else f"{pad}  "
                    if isinstance(child, (dict, list)):
                        lines.append(f"{prefix}{key}:")
                        lines.extend(emit_yaml(child, indent + 2))
                    else:
                        lines.append(f"{prefix}{key}: {emit_scalar(child)}")
                    first = False
            else:
                lines.append(f"{pad}- {emit_scalar(item)}")
        return lines
    return [f"{pad}{emit_scalar(value)}"]


def write_yaml(path: Path, data: dict) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    with open(path, "w") as f:
        f.write("\n".join(emit_yaml(data)) + "\n")


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--manifest", required=True, type=Path)
    parser.add_argument("--sha256-dir", required=True, type=Path)
    parser.add_argument("--repo", default="thecont1/srvm")
    parser.add_argument("--tag", required=True)
    parser.add_argument("--version", default=None, help="defaults to tag with leading v stripped")
    parser.add_argument("--identifier", default="thecont1.srvm")
    parser.add_argument("--out-dir", type=Path, default=Path("dist-manifests"))
    args = parser.parse_args()

    manifest = load_manifest(args.manifest)
    version = args.version or re.sub(r"^v", "", args.tag)

    scoop = scoop_manifest(manifest, args.repo, args.tag, version, args.sha256_dir)
    scoop_path = args.out_dir / "scoop" / "srvm.json"
    scoop_path.parent.mkdir(parents=True, exist_ok=True)
    with open(scoop_path, "w") as f:
        json.dump(scoop, f, indent=2)
        f.write("\n")

    winget = winget_manifests(manifest, args.repo, args.tag, version, args.sha256_dir, args.identifier)
    winget_dir = args.out_dir / "winget" / args.identifier / version
    for filename, data in winget.items():
        write_yaml(winget_dir / filename, data)

    print(f"scoop: {scoop_path}")
    for filename in winget:
        print(f"winget: {winget_dir / filename}")


if __name__ == "__main__":
    main()
