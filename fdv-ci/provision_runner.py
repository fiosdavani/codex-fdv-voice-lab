#!/usr/bin/env python3
"""Install only missing build headers privately on the disposable Ubuntu runner."""
import hashlib
import json
import os
from pathlib import Path
import subprocess

root = Path(os.environ["RUNNER_TEMP"]) / "fdv-native-deps"
root.mkdir(exist_ok=False)
downloads = root / "downloads"
downloads.mkdir()
prefix = root / "sysroot"
prefix.mkdir()
receipt = {"sudo_used": False, "system_install": False, "commands": [], "packages": []}
out = Path(os.environ["RUNNER_TEMP"]) / "fdv-receipts"
out.mkdir(exist_ok=True)


def command(argv):
    result = subprocess.run(argv, cwd=downloads, capture_output=True, text=True)
    receipt["commands"].append({"argv": argv, "rc": result.returncode,
                                "stdout": result.stdout, "stderr": result.stderr})
    (out / "NATIVE-DEPS.json").write_text(json.dumps(receipt, indent=2) + "\n")
    print(result.stdout, result.stderr, flush=True)
    result.check_returncode()
    return result.stdout.strip()


if os.geteuid() == 0:
    raise SystemExit("non-root runner required")
for tool in ("cc", "make", "cmake", "pkg-config", "dpkg-deb"):
    command([tool, "--version"])
needed = []
for module, packages in (("libcap", ["libcap-dev", "libcap2"]),
                         ("alsa", ["libasound2-dev", "libasound2t64"])):
    if subprocess.run(["pkg-config", "--exists", module]).returncode:
        needed.extend(packages)
if needed:
    # apt's existing signed package metadata selects/verifies these Ubuntu packages.
    # Download and unpack do not run maintainer scripts or mutate the system DB.
    command(["apt-get", "download", *needed])
    for package in sorted(downloads.glob("*.deb")):
        info = command(["dpkg-deb", "--field", str(package), "Package", "Version", "Architecture"])
        receipt["packages"].append({"file": package.name, "metadata": info,
            "sha256": hashlib.sha256(package.read_bytes()).hexdigest()})
        command(["dpkg-deb", "--extract", str(package), str(prefix)])
    for pc in prefix.rglob("*.pc"):
        text = pc.read_text().replace("=/usr", f"={prefix}/usr")
        pc.write_text(text)
    machine = command(["cc", "-dumpmachine"])
    libraries = [prefix / "usr/lib" / machine, prefix / "lib" / machine]
    settings = {"PKG_CONFIG_PATH": ":".join(str(path / "pkgconfig") for path in libraries),
                "CPATH": str(prefix / "usr/include"),
                "LIBRARY_PATH": ":".join(map(str, libraries)),
                "LD_LIBRARY_PATH": ":".join(map(str, libraries))}
    with Path(os.environ["GITHUB_ENV"]).open("a") as env_file:
        for key, value in settings.items():
            if os.environ.get(key):
                value += ":" + os.environ[key]
            os.environ[key] = value
            env_file.write(f"{key}={value}\n")
for module in ("libcap", "alsa"):
    command(["pkg-config", "--modversion", module])
receipt["status"] = "PASS"
(out / "NATIVE-DEPS.json").write_text(json.dumps(receipt, indent=2) + "\n")
