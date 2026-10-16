#!/usr/bin/env python3
"""Stage a local native candidate without changing the active plugin manifest."""

import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import subprocess
import tempfile
import tomllib


def digest(data):
    return hashlib.sha256(data).hexdigest()


def stage(binary, output):
    root = Path(__file__).resolve().parent.parent
    system = {"Darwin": "macos", "Linux": "linux"}.get(platform.system())
    if system is None:
        raise ValueError("native packaging is not validated on this platform")
    cargo = tomllib.loads((root / "Cargo.toml").read_text())
    version = cargo["package"]["version"]
    result = subprocess.run(
        [str(binary.resolve()), "--version"], capture_output=True, text=True,
        check=True, timeout=10,
    )
    if result.stdout.strip() != f"last-pane-guard {version}":
        raise ValueError("binary version does not match Cargo.toml")
    manifest = (root / "herdr-plugin.toml").read_text()
    original = tomllib.loads(manifest)
    replacements = {
        '["bash", "bin/guard.sh", "refresh"]': '["bin/last-pane-guard", "refresh"]',
        '["bash", "bin/guard.sh", "event"]': '["bin/last-pane-guard", "event"]',
        '["bash", "bin/confirm.sh"]': '["bin/last-pane-guard", "confirm"]',
    }
    for old, new in replacements.items():
        if old not in manifest:
            raise ValueError("source manifest entrypoint contract changed")
        manifest = manifest.replace(old, new)
    manifest = manifest.replace(
        f'version = "{original["version"]}"', f'version = "{version}"', 1,
    ).replace('platforms = ["macos", "linux"]', f'platforms = ["{system}"]', 1)
    candidate = tomllib.loads(manifest)
    if candidate["platforms"] != [system]:
        raise ValueError("candidate platform declaration is inconsistent")
    for section in ("startup", "actions", "events", "panes"):
        for entry in candidate[section]:
            if entry["command"][0] != "bin/last-pane-guard":
                raise ValueError("candidate has a non-native entrypoint")
    files = {
        "herdr-plugin.toml": manifest.encode(),
        "bin/last-pane-guard": binary.read_bytes(),
        "LICENSE": (root / "LICENSE").read_bytes(),
    }
    metadata = {
        "version": version, "platform": system, "architecture": platform.machine(),
        "sha256": {name: digest(data) for name, data in files.items()},
    }
    files["checksums.json"] = (json.dumps(metadata, indent=2, sort_keys=True) + "\n").encode()
    if output.exists() or output.is_symlink():
        if output.is_symlink() or not output.is_dir():
            raise ValueError("output is not an ordinary directory")
        actual = {str(path.relative_to(output)) for path in output.rglob("*") if path.is_file()}
        if actual != set(files) or any(
            (output / name).is_symlink() or (output / name).read_bytes() != data
            for name, data in files.items()
        ):
            raise ValueError("output already exists with different contents; choose a new directory")
        if not os.access(output / "bin/last-pane-guard", os.X_OK):
            raise ValueError("existing candidate binary is not executable")
        return "candidate unchanged"
    output.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix=".native-stage-", dir=output.parent) as temporary:
        staging = Path(temporary)
        for name, data in files.items():
            destination = staging / name
            destination.parent.mkdir(parents=True, exist_ok=True)
            destination.write_bytes(data)
        (staging / "bin/last-pane-guard").chmod(0o755)
        os.rename(staging, output)
    return "native candidate staged; not installed or published"


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    try:
        print(stage(args.binary, args.output))
    except (OSError, ValueError, subprocess.SubprocessError):
        parser.exit(1, "candidate staging failed; inputs and destination were not accepted\n")


if __name__ == "__main__":
    main()
