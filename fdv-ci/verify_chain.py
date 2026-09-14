#!/usr/bin/env python3
"""Reapply the approved patches to the full pinned Git tree, without network."""

import argparse
import hashlib
import json
from pathlib import Path
import subprocess
import sys

PIN = "6f39a47bb3b04de4c804187bfbf55edc56939aab"
BASE_TREE = "9d67c53fd490cb4e01fbd82974dded2a0aeee6dc"


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def verify(repo, out, worktree, config):
    out.mkdir(parents=True, exist_ok=True)
    receipt_path = out / "PATCH-CHAIN-RECEIPT.json"
    if receipt_path.exists() or worktree.exists():
        raise ValueError("new receipt/worktree paths required; never overwrite a prior proof")
    receipt = {"FULL_CHECKOUT_PIN": PIN, "PATCH_CHAIN_APPLY": "FAIL", "commands": [], "stages": []}

    def save():
        receipt_path.write_text(json.dumps(receipt, indent=2) + "\n")

    def run(argv, cwd=repo):
        result = subprocess.run(argv, cwd=cwd, capture_output=True, text=True, check=False)
        receipt["commands"].append({"argv": argv, "cwd": str(cwd), "rc": result.returncode,
                                    "stdout": result.stdout, "stderr": result.stderr})
        save()
        if result.returncode:
            raise RuntimeError(f"command failed rc={result.returncode}: {argv}: {result.stderr}")
        return result.stdout.strip()

    try:
        manifest = json.loads(config.read_text())
        if manifest["pin"] != PIN or manifest["base_tree"] != BASE_TREE or len(manifest["stages"]) != 3:
            raise ValueError("wrong pinned chain manifest")
        if run(["git", "rev-parse", PIN + "^{tree}"]) != BASE_TREE:
            raise ValueError("full upstream tree identity mismatch")
        if run(["git", "status", "--porcelain", "--untracked-files=no"]):
            raise ValueError("checkout must be pristine before proof")
        receipt["HEAD"] = run(["git", "rev-parse", "HEAD"])
        receipt["base_tracked_files"] = len(run(["git", "ls-tree", "-r", "--name-only", PIN]).splitlines())
        if receipt["base_tracked_files"] <= 48:
            raise ValueError("selective import is not the full source")
        run(["git", "-c", "core.hooksPath=/dev/null", "worktree", "add", "--detach", str(worktree), PIN])
        parent = PIN
        for number, stage in enumerate(manifest["stages"], 1):
            patch = config.parent / stage["patch"]
            if sha(patch) != stage["patch_sha256"]:
                raise ValueError(f"patch {number} SHA mismatch")
            parents = run(["git", "rev-list", "--parents", "-n", "1", stage["commit"]]).split()
            if parents != [stage["commit"], parent]:
                raise ValueError(f"stage {number} has unexpected ancestry")
            if run(["git", "rev-parse", stage["commit"] + "^{tree}"]) != stage["tree"]:
                raise ValueError(f"stage {number} tree identity mismatch")
            run(["git", "apply", "--check", str(patch)], worktree)
            run(["git", "apply", str(patch)], worktree)
            files = stage["files"]
            run(["git", "add", "--force", "--", *[entry["path"] for entry in files]], worktree)
            tree = run(["git", "write-tree"], worktree)
            if tree != stage["tree"]:
                raise ValueError(f"stage {number}: actual full-tree patch application differs")
            for entry in files:
                if sha(worktree / entry["path"]) != entry["sha256"]:
                    raise ValueError(f"stage {number}: candidate file SHA mismatch {entry['path']}")
            receipt["stages"].append({"stage": number, "commit": stage["commit"], "tree": tree,
                                      "patch_sha256": stage["patch_sha256"], "files_verified": len(files),
                                      "apply_check_rc": 0, "apply_rc": 0, "tree_byte_match": True})
            parent = stage["commit"]
        run(["git", "merge-base", "--is-ancestor", parent, "HEAD"])
        corrections = {item["path"]: item for item in manifest.get("compiler_corrections", [])}
        changed = run(["git", "diff", "--name-only", parent, "HEAD"]).splitlines()
        non_ci = {path for path in changed if not path.startswith("fdv-ci/")
                  and path != ".github/workflows/fdv-voice-rust-gate.yml"}
        if non_ci != set(corrections):
            raise ValueError(f"unrecorded source change after approved chain: {non_ci ^ set(corrections)}")
        for path, item in corrections.items():
            if not path.startswith("codex-rs/") or not item["reason"] or not item["rust_receipt"]:
                raise ValueError("compiler correction must identify an observed Rust defect")
            if sha(repo / path) != item["sha256"]:
                raise ValueError("compiler correction source hash mismatch")
        receipt["compiler_corrections"] = list(corrections.values())
        receipt["PATCH_CHAIN_APPLY"] = "PASS"
        receipt["chain_manifest_sha256"] = sha(config)
        save()
        print(json.dumps({key: value for key, value in receipt.items() if key != "commands"}, indent=2))
        return 0
    except Exception as error:
        receipt["error"] = f"{type(error).__name__}: {error}"
        save()
        print(receipt["error"], file=sys.stderr)
        return 1


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repo", type=Path, required=True)
    parser.add_argument("--out", type=Path, required=True)
    parser.add_argument("--worktree", type=Path, required=True)
    args = parser.parse_args()
    return verify(args.repo.resolve(), args.out.resolve(), args.worktree.resolve(),
                  args.repo.resolve() / "fdv-ci" / "CHAIN.json")


if __name__ == "__main__":
    raise SystemExit(main())
