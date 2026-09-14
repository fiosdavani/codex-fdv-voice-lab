#!/usr/bin/env python3
"""Always expose a bounded mechanical-gate receipt, including setup failures."""
import hashlib
import json
import os
from pathlib import Path
import sys

out = Path(os.environ["RUNNER_TEMP"]) / "fdv-receipts"
out.mkdir(exist_ok=True)
path = out / "RUST-RECEIPT.json"
if not path.exists():
    chain_path = out / "PATCH-CHAIN-RECEIPT.json"
    chain = json.loads(chain_path.read_text()) if chain_path.exists() else {}
    result = {"FULL_CHECKOUT_PIN": "6f39a47bb3b04de4c804187bfbf55edc56939aab",
              "PATCH_CHAIN_APPLY": chain.get("PATCH_CHAIN_APPLY", "NOT_RUN"),
              "RUSTC_VERSION": "NOT_RUN", "CARGO_VERSION": "NOT_RUN",
              "FMT": "NOT_RUN", "CARGO_CHECK": "NOT_RUN", "CLIPPY": "NOT_RUN",
              "TESTS_DISCOVERED": 0, "TESTS_EXECUTED": 0, "TESTS_PASSED": 0,
              "TESTS_FAILED": 0, "EXACT_FAILING_TESTS": [], "RUST_GATE": "BLOCKED_SETUP",
              "errors": ["Rust runner was not reached; inspect setup step outcomes"]}
    path.write_text(json.dumps(result, indent=2) + "\n")
result = json.loads(path.read_text())
result["RUN_URL"] = f"https://github.com/{os.environ['GITHUB_REPOSITORY']}/actions/runs/{os.environ['GITHUB_RUN_ID']}"
result["RUN_SHA"] = os.environ["GITHUB_SHA"]
result["STEP_OUTCOMES"] = {key: os.environ.get(key, "unknown") for key in
                           ("CHAIN_OUTCOME", "TOOLCHAIN_OUTCOME", "TOOLS_OUTCOME", "GATE_OUTCOME")}
result["BUILD_ENV"] = {key: os.environ.get(key) for key in
                       ("CARGO_BUILD_JOBS", "CARGO_INCREMENTAL", "CARGO_PROFILE_DEV_DEBUG", "CARGO_PROFILE_TEST_DEBUG")}
path.write_text(json.dumps(result, indent=2) + "\n")
sha = hashlib.sha256(path.read_bytes()).hexdigest()
(out / "RUST-RECEIPT.sha256").write_text(f"{sha}  RUST-RECEIPT.json\n")
print("FDV_RUST_RECEIPT_BEGIN")
print(path.read_text())
print("FDV_RUST_RECEIPT_END")
print("RUST_RECEIPT_SHA256=" + sha)
# The full formatter delta remains obtainable via the native GitHub log tool.
patch = out / "fmt-diff.stdout.log"
if patch.exists() and patch.stat().st_size <= 2_000_000:
    print("FDV_FORMAT_PATCH_BEGIN")
    print(patch.read_text())
    print("FDV_FORMAT_PATCH_END")
with Path(os.environ["GITHUB_STEP_SUMMARY"]).open("a") as summary:
    summary.write("```json\n" + json.dumps({key: value for key, value in result.items()
                    if key.isupper()}, indent=2) + "\n```\n")
sys.exit(0 if result.get("RUST_GATE") == "PASS" else 1)
