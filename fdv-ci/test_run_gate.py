"""Synthetic receipt-parser controls. These are NOT Rust test execution."""

import json
import unittest

from run_gate import EvidenceError, parse_discovery, parse_junit, summarize, unique_object, verify_expected


def discovery(names=("a", "b"), ignored=()):
    return {"test-count": len(names), "rust-suites": {"codex-core": {
        "package-name": "codex-core", "binary-id": "codex-core", "kind": "lib",
        "status": "listed", "testcases": {
            name: {"ignored": name in ignored, "filter-match": {"status": "matches"}}
            for name in names
        },
    }}}


def junit(cases):
    return ('<testsuites><testsuite name="codex-core">' + cases + '</testsuite></testsuites>').encode()


def case(name, content=""):
    return f'<testcase classname="codex-core" name="{name}">{content}</testcase>'


class ReceiptParserTests(unittest.TestCase):
    def setUp(self):
        self.discovered = parse_discovery(discovery())

    def test_two_real_cases_pass(self):
        result = parse_junit(junit(case("a") + case("b")), self.discovered)
        summary = summarize(self.discovered, result)
        self.assertTrue(summary["PASS"])
        self.assertEqual((summary["discovered"], summary["executed"], summary["passed"], summary["failed"]), (2, 2, 2, 0))

    def test_zero_discovery_is_failure(self):
        with self.assertRaisesRegex(EvidenceError, "zero tests"):
            parse_discovery(discovery(()))

    def test_filtered_mismatches_do_not_count(self):
        source = discovery()
        source["rust-suites"]["codex-core"]["testcases"]["b"]["filter-match"]["status"] = "mismatch"
        self.assertEqual(len(parse_discovery(source)), 1)

    def test_all_filtered_is_not_pass(self):
        source = discovery(("a",))
        source["rust-suites"]["codex-core"]["testcases"]["a"]["filter-match"]["status"] = "mismatch"
        with self.assertRaises(EvidenceError):
            parse_discovery(source)

    def test_missing_suite_is_failure(self):
        with self.assertRaises(EvidenceError):
            parse_discovery({"test-count": 58})

    def test_skipped_binary_does_not_claim_discovery(self):
        source = discovery()
        source["rust-suites"]["codex-core"]["status"] = "skipped"
        with self.assertRaises(EvidenceError):
            parse_discovery(source)

    def test_unknown_filter_is_failure(self):
        source = discovery()
        source["rust-suites"]["codex-core"]["testcases"]["a"]["filter-match"]["status"] = "maybe"
        with self.assertRaises(EvidenceError):
            parse_discovery(source)

    def test_duplicate_json_key_rejected(self):
        with self.assertRaisesRegex(EvidenceError, "duplicate JSON key"):
            json.loads('{"testcases":{"a":{},"a":{}}}', object_pairs_hook=unique_object)

    def test_missing_case_is_not_executed_or_passed(self):
        results = parse_junit(junit(case("a")), self.discovered)
        summary = summarize(self.discovered, results)
        self.assertFalse(summary["PASS"])
        self.assertEqual(summary["executed"], 1)
        self.assertEqual(summary["missing_results"], [("codex-core", "b")])

    def test_empty_junit_is_zero_execution(self):
        summary = summarize(self.discovered, parse_junit(junit(""), self.discovered))
        self.assertFalse(summary["PASS"])
        self.assertEqual(summary["executed"], 0)

    def test_declared_xml_total_does_not_manufacture_execution(self):
        source = junit(case("a")).replace(b'<testsuite name=', b'<testsuite tests="999" name=')
        self.assertEqual(summarize(self.discovered, parse_junit(source, self.discovered))["executed"], 1)

    def test_skipped_is_not_executed(self):
        results = parse_junit(junit(case("a", '<skipped message="ignored"/>') + case("b")), self.discovered)
        summary = summarize(self.discovered, results)
        self.assertFalse(summary["PASS"])
        self.assertEqual((summary["executed"], summary["passed"], summary["failed"]), (1, 1, 0))
        self.assertEqual(summary["skipped"], [("codex-core", "a")])

    def test_ignored_discovery_never_satisfies_gate(self):
        discovered = parse_discovery(discovery(ignored=("a",)))
        summary = summarize(discovered, parse_junit(junit(case("b")), discovered))
        self.assertFalse(summary["PASS"])
        self.assertEqual(summary["ignored"], [("codex-core", "a")])

    def test_failure_name_and_count_preserved(self):
        results = parse_junit(junit(case("a", '<failure type="assert">bad</failure>') + case("b")), self.discovered)
        summary = summarize(self.discovered, results)
        self.assertEqual((summary["executed"], summary["passed"], summary["failed"]), (2, 1, 1))
        self.assertEqual(summary["failing_tests"], [("codex-core", "a")])

    def test_error_is_execution_failure(self):
        results = parse_junit(junit(case("a", '<error type="timeout"/>') + case("b")), self.discovered)
        self.assertEqual(summarize(self.discovered, results)["failed"], 1)

    def test_spawn_failure_does_not_claim_test_execution(self):
        results = parse_junit(junit(case("a", '<error type="execution failure"/>') + case("b")), self.discovered)
        summary = summarize(self.discovered, results)
        self.assertFalse(summary["PASS"])
        self.assertEqual((summary["executed"], summary["passed"], summary["failed"]), (1, 1, 0))
        self.assertEqual(summary["launch_errors"], [("codex-core", "a")])

    def test_process_leaked_handles_counts_as_executed_failure(self):
        results = parse_junit(junit(case("a", '<error type="test passed but leaked handles"/>') + case("b")), self.discovered)
        summary = summarize(self.discovered, results)
        self.assertEqual((summary["executed"], summary["passed"], summary["failed"]), (2, 1, 1))
        self.assertEqual(summary["launch_errors"], [])

    def test_duplicate_junit_is_not_double_counted(self):
        with self.assertRaisesRegex(EvidenceError, "duplicate JUnit"):
            parse_junit(junit(case("a") + case("a")), self.discovered)

    def test_unknown_case_identity_is_rejected(self):
        with self.assertRaisesRegex(EvidenceError, "not discovered"):
            parse_junit(junit(case("c")), self.discovered)

    def test_wrong_binary_identity_is_rejected(self):
        with self.assertRaises(EvidenceError):
            parse_junit(junit(case("a")).replace(b'classname="codex-core"', b'classname="other"'), self.discovered)

    def test_retries_cannot_inflate_pass_count(self):
        for tag in ("flakyFailure", "rerunFailure", "flakyError", "rerunError"):
            with self.subTest(tag=tag), self.assertRaisesRegex(EvidenceError, "retry"):
                parse_junit(junit(case("a", f'<{tag}/>')), self.discovered)

    def test_conflicting_skip_and_failure_rejected(self):
        with self.assertRaises(EvidenceError):
            parse_junit(junit(case("a", '<skipped/><failure/>')), self.discovered)

    def test_expected_missing_source_name_rejected(self):
        with self.assertRaisesRegex(EvidenceError, "absent"):
            verify_expected(self.discovered, [{"crate": "codex-core", "test_name": "missing"}])

    def test_expected_wrong_crate_rejected(self):
        with self.assertRaises(EvidenceError):
            verify_expected(self.discovered, [{"crate": "other", "test_name": "a"}])

    def test_expected_extra_tests_do_not_replace_missing_test(self):
        verify_expected(self.discovered, [{"crate": "codex-core", "test_name": "a"}])
        with self.assertRaises(EvidenceError):
            verify_expected(self.discovered, [{"crate": "codex-core", "test_name": "not-a"}])


if __name__ == "__main__":
    unittest.main(verbosity=2)
