#!/usr/bin/env python3
"""Mechanical Rust gate. Runs only against the separate, approved candidate checkout.

No model, Voice, Windows, credentials, deploy, or production API is used. Cargo may
fetch public dependencies under the GitHub runner's policy. Each receipt records
commands actually invoked; source inventory is never counted as execution.
"""

import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import time
import xml.etree.ElementTree as ET

PIN = "6f39a47bb3b04de4c804187bfbf55edc56939aab"
VERSION = "1.95.0"
PACKAGES = (
    "codex-state", "codex-queue-extension", "codex-core", "codex-app-server",
    "codex-thread-store", "codex-extension-api",
)
SELECTIONS = [
    ("state_queue", ["-p", "codex-state", "--lib", "-E",
                     "test(/^runtime::queued_items::tests::/)"], {"codex-state"}),
    ("queue_service", ["-p", "codex-queue-extension", "--test", "queue_service"],
     {"codex-queue-extension"}),
    ("core_voice", ["-p", "codex-core", "--lib", "-E",
                    "test(/^realtime_voice_/) | test(/^realtime_conversation::/)"],
     {"codex-core"}),
    ("core_turn_input", ["-p", "codex-core", "--lib", "-E",
                         "test(/^session::turn_input::tests::/)"], set()),
]


class EvidenceError(ValueError):
    pass


def unique_object(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise EvidenceError(f"duplicate JSON key: {key}")
        result[key] = value
    return result


def load_json(path):
    return json.loads(Path(path).read_text(), object_pairs_hook=unique_object)


def digest(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def parse_discovery(document):
    """Read nextest's compiled list, excluding explicit filter mismatches."""
    suites = document.get("rust-suites")
    if not isinstance(suites, dict):
        raise EvidenceError("missing nextest rust-suites")
    selected = {}
    for suite_id, suite in suites.items():
        if suite.get("status") != "listed":
            raise EvidenceError(f"suite not listed: {suite_id}")
        binary = suite.get("binary-id")
        package = suite.get("package-name")
        if not binary or not package or binary != suite_id:
            raise EvidenceError(f"invalid suite identity: {suite_id}")
        for name, case in suite.get("testcases", {}).items():
            status = case.get("filter-match", {}).get("status")
            if status == "mismatch":
                continue
            if status != "matches" or not isinstance(case.get("ignored"), bool):
                raise EvidenceError(f"unknown filter/ignored state: {suite_id}::{name}")
            key = (binary, name)
            if key in selected:
                raise EvidenceError(f"duplicate discovered test: {key}")
            selected[key] = {"package": package, "ignored": case["ignored"]}
    if not selected:
        raise EvidenceError("zero tests matched the compiled selection")
    return selected


def parse_junit(xml_bytes, discovered):
    """Count case outcomes, not log words or declared XML totals; retries disabled."""
    root = ET.fromstring(xml_bytes)
    if root.tag not in {"testsuites", "testsuite"}:
        raise EvidenceError("unexpected JUnit root")
    suites = [root] if root.tag == "testsuite" else root.findall("testsuite")
    results = {}
    for suite in suites:
        for case in suite.findall("testcase"):
            key = (case.get("classname") or suite.get("name"), case.get("name"))
            if key not in discovered:
                raise EvidenceError(f"JUnit test not discovered: {key}")
            if key in results:
                raise EvidenceError(f"duplicate JUnit test: {key}")
            if any(case.find(tag) is not None for tag in
                   ("rerunFailure", "flakyFailure", "rerunError", "flakyError")):
                raise EvidenceError(f"unexpected retry evidence: {key}")
            skipped = case.find("skipped") is not None
            failure = case.find("failure") is not None or case.find("error") is not None
            if skipped and failure:
                raise EvidenceError(f"conflicting JUnit outcome: {key}")
            error = case.find("error")
            launch_error = error is not None and error.get("type") == "execution failure"
            results[key] = ("LAUNCH_ERROR" if launch_error else "SKIPPED" if skipped
                            else "FAILED" if failure else "PASSED")
    return results


def summarize(discovered, results):
    missing = sorted(set(discovered) - set(results))
    skipped = sorted(key for key, outcome in results.items() if outcome == "SKIPPED")
    failed = sorted(key for key, outcome in results.items() if outcome == "FAILED")
    launch_errors = sorted(key for key, outcome in results.items() if outcome == "LAUNCH_ERROR")
    ignored = sorted(key for key, value in discovered.items() if value["ignored"])
    passed = sum(value == "PASSED" for value in results.values())
    return {
        "discovered": len(discovered), "executed": passed + len(failed),
        "passed": passed, "failed": len(failed), "skipped": skipped,
        "ignored": ignored, "missing_results": missing, "failing_tests": failed,
        "launch_errors": launch_errors,
        "PASS": bool(discovered) and not (missing or skipped or failed or ignored or launch_errors),
    }


def verify_expected(discovered, expected):
    actual = {(value["package"], name) for (_, name), value in discovered.items()}
    required = {(case["crate"], case["test_name"]) for case in expected}
    missing = sorted(required - actual)
    if missing:
        raise EvidenceError(f"expected source tests absent from compiled discovery: {missing}")


class Gate:
    def __init__(self, repo, out):
        self.repo, self.out = repo, out
        self.rust = repo / "codex-rs"
        self.env = dict(os.environ)
        # Keep runner artifacts inside its disposable checkout. Do not override
        # Codex sandbox guards, or infer that a skipped guarded test passed.
        self.env["CARGO_TARGET_DIR"] = str(self.rust / "target")
        self.env["CARGO_TERM_COLOR"] = "never"
        self.env["RUST_MIN_STACK"] = "8388608"
        self.receipt = {
            "schema_version": 1, "FULL_CHECKOUT_PIN": PIN, "PATCH_CHAIN_APPLY": "NC",
            "RUSTC_VERSION": "NOT_RUN", "CARGO_VERSION": "NOT_RUN",
            "FMT": "NOT_RUN", "CARGO_CHECK": "NOT_RUN", "CLIPPY": "NOT_RUN",
            "TESTS_DISCOVERED": 0, "TESTS_EXECUTED": 0, "TESTS_PASSED": 0,
            "TESTS_FAILED": 0, "EXACT_FAILING_TESTS": [], "TESTS_SKIPPED": [],
            "TESTS_MISSING_RESULTS": [], "ABORT_SIX": "NOT_RUN", "EXPECTED_58": "NOT_RUN",
            "RUST_GATE": "FAIL", "commands": [], "selections": [], "errors": [],
            "WINDOWS": "NO", "VOICE": "NO", "ELEVENLABS": "NO", "PRODUCTION": "NO",
            "COUNTING_RULE": "Distinct compiled identities; executed excludes skips/missing/spawn failures; no retries. EXACT_FAILING_TESTS includes launch errors listed separately.",
            "planned_test_selections": [{"name": name, "arguments": argv}
                                         for name, argv, _ in SELECTIONS],
            "SANDBOX_GUARDS_PRESENT": [name for name in ("CODEX_SANDBOX", "CODEX_SANDBOX_NETWORK_DISABLED")
                                       if name in os.environ],
        }
        self.discovered, self.results = {}, {}
        self.expected, self.abort_six = [], []
        self.save()

    def save(self):
        stats = summarize(self.discovered, self.results)
        for field in ("discovered", "executed", "passed", "failed"):
            self.receipt[f"TESTS_{field.upper()}"] = stats[field]
        self.receipt["EXACT_FAILING_TESTS"] = sorted(stats["failing_tests"] + stats["launch_errors"])
        self.receipt["TESTS_LAUNCH_ERRORS"] = stats["launch_errors"]
        self.receipt["TESTS_SKIPPED"] = stats["skipped"]
        self.receipt["TESTS_MISSING_RESULTS"] = stats["missing_results"]
        self.receipt["discovered_tests"] = [
            {"binary_id": binary, "test_name": name, **data}
            for (binary, name), data in sorted(self.discovered.items())
        ]
        self.receipt["test_outcomes"] = [
            {"binary_id": binary, "test_name": name, "outcome": outcome}
            for (binary, name), outcome in sorted(self.results.items())
        ]
        if self.expected:
            self.receipt["EXPECTED_58"] = self.expected_outcomes(self.expected)
            self.receipt["ABORT_SIX"] = self.expected_outcomes(self.abort_six)
        (self.out / "RUST-RECEIPT.json").write_text(json.dumps(self.receipt, indent=2) + "\n")

    def command(self, label, argv, cwd=None):
        stdout, stderr = self.out / f"{label}.stdout.log", self.out / f"{label}.stderr.log"
        start = time.time()
        entry = {"name": label, "argv": argv, "cwd": str(cwd or self.rust),
                 "started_at_unix": start, "rc": None}
        self.receipt["commands"].append(entry)
        self.save()
        print(f"::group::{label}", flush=True)
        try:
            with stdout.open("wb") as out, stderr.open("wb") as err:
                completed = subprocess.run(argv, cwd=cwd or self.rust, env=self.env,
                                           stdout=out, stderr=err, check=False)
                entry["rc"] = completed.returncode
        except OSError as exc:
            entry.update(rc=127, launch_error=str(exc))
        finally:
            entry.update(elapsed_seconds=round(time.time() - start, 3),
                         stdout=str(stdout.name), stderr=str(stderr.name))
            for path in (stdout, stderr):
                if path.exists():
                    entry[path.suffixes[-2].lstrip(".") + "_sha256"] = digest(path)
                    # Bounded console tail; complete raw bytes stay in artifacts.
                    print(path.read_text(errors="replace")[-6000:], flush=True)
            print(f"rc={entry['rc']}\n::endgroup::", flush=True)
            self.save()
        return entry["rc"], stdout

    def run(self, chain_path, inventory_path):
        inventory = load_json(inventory_path)
        expected = inventory["all_58_expected_tests"]
        abort_six = inventory["six_abort_micro_delta_expected_tests"]
        if len(expected) != 58 or len(abort_six) != 6:
            raise EvidenceError("inventory expected 58 tests and six abort tests")
        self.expected, self.abort_six = expected, abort_six
        self.receipt["inventory_sha256"] = digest(inventory_path)
        chain = load_json(chain_path)
        if chain.get("FULL_CHECKOUT_PIN") != PIN or chain.get("PATCH_CHAIN_APPLY") != "PASS":
            raise EvidenceError("approved patch chain receipt missing or not PASS at exact pin")
        self.receipt["PATCH_CHAIN_APPLY"] = "PASS"
        self.receipt["patch_chain_receipt_sha256"] = digest(chain_path)
        rc, _ = self.command("pin-ancestor", ["git", "merge-base", "--is-ancestor", PIN, "HEAD"], self.repo)
        if rc:
            raise EvidenceError("pinned upstream commit is not an ancestor")
        rc, clean = self.command("initial-status", ["git", "status", "--porcelain", "--untracked-files=no"], self.repo)
        if rc or clean.read_text().strip():
            raise EvidenceError("tracked working tree must be clean before mechanical format correction")
        for label, executable in (("RUSTC_VERSION", "rustc"), ("CARGO_VERSION", "cargo")):
            rc, stdout = self.command(label.lower(), [executable, "--version"])
            value = stdout.read_text().strip() if stdout.exists() else "LAUNCH_FAILED"
            self.receipt[label] = value
            if rc or not value.startswith(f"{executable} {VERSION} "):
                raise EvidenceError(f"wrong/missing {executable}; expected exact {VERSION}: {value}")
        for executable in ("just", "cargo-nextest"):
            if shutil.which(executable) is None:
                raise EvidenceError(f"required preinstalled tool missing: {executable}")
        rc, version_log = self.command("nextest-version", ["cargo", "nextest", "--version"])
        self.receipt["NEXTEST_VERSION"] = version_log.read_text().strip()
        if rc or self.receipt["NEXTEST_VERSION"].split()[:2] != ["cargo-nextest", "0.9.103"]:
            raise EvidenceError("nextest must be 0.9.103, matching the reviewed metadata/JUnit contract")
        rc, version_log = self.command("just-version", ["just", "--version"], self.repo)
        self.receipt["JUST_VERSION"] = version_log.read_text().strip()
        if rc:
            raise EvidenceError("just is not executable")
        fmt = ["cargo", "fmt", "--all", "--", "--config", "imports_granularity=Item"]
        rc, _ = self.command("fmt", [*fmt, "--check"])
        self.receipt["FMT_INITIAL"] = "PASS" if rc == 0 else "FAIL"
        if rc:
            fix_rc, _ = self.command("fmt-correction", fmt)
            diff_rc, patch = self.command("fmt-diff", ["git", "diff", "--binary"], self.repo)
            self.receipt["FORMAT_CORRECTION"] = {
                "rc": fix_rc, "diff_rc": diff_rc, "patch": patch.name,
                "sha256": digest(patch), "scope": "cargo fmt only; no commit or push",
            }
            if fix_rc or diff_rc:
                raise EvidenceError("mechanical rustfmt correction failed")
            rc, _ = self.command("fmt-recheck", [*fmt, "--check"])
        self.receipt["FMT"] = "PASS" if rc == 0 else "FAIL"
        self.receipt["compiled_test_sources"] = []
        for module in inventory["modules"]:
            suffix = module["source_file"].partition("/codex-rs/")[2]
            if not suffix:
                raise EvidenceError("inventory source path does not identify codex-rs")
            path = self.rust / suffix
            self.receipt["compiled_test_sources"].append({
                "path": str(path.relative_to(self.repo)), "sha256": digest(path),
                "approved_pre_format_sha256": module["source_sha256"],
            })
        packages = [arg for package in PACKAGES for arg in ("-p", package)]
        rc, _ = self.command("cargo-check", ["cargo", "check", "--locked", "--tests", *packages])
        self.receipt["CARGO_CHECK"] = "PASS" if rc == 0 else "FAIL"
        if rc:
            self.receipt["CLIPPY"] = "NOT_RUN_CHECK_FAILED"
            raise EvidenceError("cargo check failed; do not claim compiler discovery or test execution")
        rc, _ = self.command("clippy", ["cargo", "clippy", "--locked", "--tests", *packages, "--", "-D", "warnings"])
        self.receipt["CLIPPY"] = "PASS" if rc == 0 else "FAIL"
        # Apart from the recorded rustfmt-only delta, target source is unchanged.
        for name, arguments, expected_packages in SELECTIONS:
            entry = {"name": name, "status": "NOT_RUN"}
            self.receipt["selections"].append(entry)
            rc, stdout = self.command(name + "-list", ["cargo", "nextest", "list", "--locked",
                                                      *arguments, "--message-format", "json"])
            if rc:
                entry["status"] = "DISCOVERY_FAILED"
                self.save()
                raise EvidenceError(f"compiled discovery failed for {name}")
            discovered = parse_discovery(load_json(stdout))
            required = [test for test in expected if test["crate"] in expected_packages]
            verify_expected(discovered, required)
            if set(discovered) & set(self.discovered):
                raise EvidenceError(f"overlapping selection {name}; counting would double count")
            self.discovered.update(discovered)
            entry.update(status="DISCOVERED", discovered=len(discovered))
            self.save()
            # The pinned default profile explicitly enables junit.xml. Its local
            # child does not inherit that path on nextest 0.9.103. An explicit
            # profile argument overrides the recipe's NEXTEST_PROFILE=local.
            junit = self.rust / "target" / "nextest" / "default" / "junit.xml"
            if junit.exists():
                junit.unlink()  # Only the previous runner-generated report; never source.
            rc, _ = self.command(name + "-run", ["just", "test", "--locked", *arguments,
                "--profile", "default", "--retries", "0", "--test-threads", "2"])
            entry["run_rc"] = rc
            if not junit.is_file():
                entry["status"] = "NO_JUNIT"
                self.receipt["errors"].append(f"{name}: test command rc={rc}; no fresh JUnit")
                self.save()
                continue
            copied = self.out / (name + ".junit.xml")
            shutil.copyfile(junit, copied)
            results = parse_junit(copied.read_bytes(), discovered)
            self.results.update(results)
            summary = summarize(discovered, results)
            entry.update(summary)
            entry["status"] = "PASS" if summary["PASS"] and rc == 0 else "FAIL"
            self.save()
        verify_expected(self.discovered, expected)
        self.receipt["EXPECTED_58"] = self.expected_outcomes(expected)
        self.receipt["ABORT_SIX"] = self.expected_outcomes(abort_six)
        all_pass = (all(self.receipt[key] == "PASS" for key in ("FMT", "CARGO_CHECK", "CLIPPY",
                    "EXPECTED_58", "ABORT_SIX")) and all(
                    selection["status"] == "PASS" for selection in self.receipt["selections"]))
        self.receipt["RUST_GATE"] = "PASS" if all_pass else "FAIL"
        self.save()
        return 0 if all_pass else 1

    def expected_outcomes(self, expected):
        names = {(item["crate"], item["test_name"]) for item in expected}
        seen = {(data["package"], key[1]) for key, data in self.discovered.items()
                if key in self.results}
        passed = {(data["package"], key[1]) for key, data in self.discovered.items()
                  if self.results.get(key) == "PASSED"}
        if names <= passed:
            return "PASS"
        if names & (seen - passed):
            return "FAIL"
        return "PENDING" if names & seen else "NOT_RUN"


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repo", required=True, type=Path)
    parser.add_argument("--out", required=True, type=Path)
    parser.add_argument("--chain-receipt", type=Path)
    parser.add_argument("--inventory", type=Path, default=Path(__file__).with_name("TEST-INVENTORY.json"))
    args = parser.parse_args()
    repo, out = args.repo.resolve(), args.out.resolve()
    out.mkdir(parents=True, exist_ok=True)
    if (out / "RUST-RECEIPT.json").exists():
        parser.error("receipt destination already exists; preserve earlier attempt, choose new --out")
    gate = Gate(repo, out)
    try:
        return gate.run(args.chain_receipt or out / "PATCH-CHAIN-RECEIPT.json", args.inventory)
    except Exception as exc:
        gate.receipt["errors"].append(f"{type(exc).__name__}: {exc}")
        gate.save()
        print(f"Rust gate incomplete: {exc}", file=sys.stderr)
        return 1
    finally:
        receipt = out / "RUST-RECEIPT.json"
        (out / "RUST-RECEIPT.sha256").write_text(f"{digest(receipt)}  RUST-RECEIPT.json\n")


if __name__ == "__main__":
    raise SystemExit(main())
