#!/usr/bin/env python3
"""Bounded P0 static diagnostics. No JavaScript execution or patch application.

Literal equality is measured exactly. Structural candidates are NEVER promoted
to equivalence by this diagnostic scanner; review must inspect the byte contexts.
The small lexer is not a JavaScript parser. Its scope boundaries are explicitly
diagnostic, and lexical ambiguities remain NC instead of becoming authority.
"""

import hashlib
import json
from pathlib import Path
import re
import sys

BASE = Path(__file__).absolute().parent
TARGETS = {
    "renderer": ("webview/assets/app-initial-d9bed9d614d8.js",
                 "7c3a89e7e224f76031b45a88f72af8cd60f0c3d47aac9ca34b2c70e11dfe9867"),
    "main": (".vite/build/main-D8abTQQE.js",
             "b55be874a9b5a262c09a7945df38cec9b0ce8f14bd584ef73d6feca301ed90b4"),
}
MAX_INPUT = 64 * 1024**2
MAX_OUTPUT = 200_000
CONTEXT_BYTES = 900
IDENT = rb"[A-Za-z_$][A-Za-z0-9_$]*"
TOKEN = re.compile(rb"\s+|" + IDENT + rb"|[0-9]+(?:\.[0-9]+)?|=>|\?\.|\+\+|--|\S")
REGEX_PREFIX_WORDS = {b"return", b"throw", b"case", b"delete", b"void", b"typeof", b"yield", b"await", b"new", b"in", b"of"}
ROLE_SPECS = {
    "OWNER_CLASS_EQUIVALENT": ("renderer", ("applyRealtimeMuteState", "handleRealtimeClosed", "resetRealtimeState", "start", "stop"), 3),
    "RUNTIME_CLASS_EQUIVALENT": ("renderer", ("setOutputMuted", "prepareWebRtcSession", "dispose", "terminate", "appendText"), 4),
    "SINK_CLASS_EQUIVALENT": ("renderer", ("setOutputAudioMuted", "setInputAudioMuted", "getOutputStream", "refreshMicrophoneInput", "start"), 4),
    "CLAIM_COORDINATOR_EQUIVALENT": ("main", ("claim", "publish", "release", "control", "subscribe"), 4),
    "PRESENTATION_COORDINATOR_EQUIVALENT": ("main", ("registerSurface", "getSnapshot", "requestSurface", "reportToast", "subscribe"), 4),
}
# These are diagnostic search clues, not semantic matches or replacement text.
PROBES = {
    1: ("outputAudioMuted",), 2: ("autoplay", "muted"),
    3: ("ontrack", "outputStream"), 4: ("ontrack", "outputStream"),
    5: ("outputStream",), 6: ("play",), 7: ("setOutputAudioMuted",),
    8: ("setOutputMuted",), 9: ("outputAudioMuted",),
    10: ("onRealtimeEventMessage", "safeParse"),
    11: ("onSessionInitialized", "safeParse"),
    12: ("thread/realtime/sdp",), 13: ("terminate", "stop"),
    14: ("dispose",), 15: ("initiallyMuted",),
    16: ("initiallyInputMuted", "preparingRuntime"),
    17: ("initiallyOutputMuted",), 18: ("setOutputMuted", "setControlHandler"),
    19: ("getClaimId",), 20: ("refreshMicrophoneInput",),
    21: ("applyRealtimeMuteState",), 22: ("view.dom", "useEffect"),
}


def sha(raw):
    return hashlib.sha256(raw).hexdigest()


def require(ok, message):
    if not ok:
        raise ValueError(message)


def read_small(path, cap):
    require(not path.is_symlink() and path.is_file(), "UNSAFE_OR_MISSING_INPUT:" + str(path))
    require(path.stat().st_size <= cap, "INPUT_SIZE_CAP:" + str(path))
    raw = path.read_bytes()
    require(len(raw) <= cap, "INPUT_SIZE_CAP")
    return raw


def literal_end(raw, start, quote):
    pos = start + 1
    while pos < len(raw):
        if raw[pos] == 92:
            pos += 2
        elif raw[pos] == quote:
            return pos + 1
        else:
            pos += 1
    return len(raw)


def regex_end(raw, start):
    pos, in_set = start + 1, False
    while pos < len(raw):
        char = raw[pos]
        if char in (10, 13):
            return None
        if char == 92:
            pos += 2
            continue
        if char == 91:
            in_set = True
        elif char == 93:
            in_set = False
        elif char == 47 and not in_set:
            pos += 1
            while pos < len(raw) and chr(raw[pos]).isalpha():
                pos += 1
            return pos
        pos += 1
    return None


def template_end(raw, start):
    """Skip nested template substitutions without evaluating their contents."""
    pos = start + 1
    while pos < len(raw):
        if raw[pos] == 92:
            pos += 2
        elif raw[pos] == 96:
            return pos + 1
        elif raw[pos:pos + 2] == b"${":
            pos = expression_end(raw, pos + 2)
        else:
            pos += 1
    return len(raw)


def expression_end(raw, start):
    pos, depth, previous = start, 1, b"="
    while pos < len(raw):
        token = TOKEN.match(raw, pos)
        if token is None:
            return len(raw)
        value, end = token.group(), token.end()
        if value.isspace():
            pos = end
            continue
        skipped = skip_literal(raw, pos, previous)
        if skipped is not None:
            pos = skipped
            previous = b"literal"
            continue
        if value == b"{":
            depth += 1
        elif value == b"}":
            depth -= 1
            if depth == 0:
                return end
        previous, pos = value, end
    return len(raw)


def skip_literal(raw, pos, previous):
    char = raw[pos]
    if char in (34, 39):
        return literal_end(raw, pos, char)
    if char == 96:
        return template_end(raw, pos)
    if raw[pos:pos + 2] == b"//":
        end = raw.find(b"\n", pos + 2)
        return len(raw) if end < 0 else end
    if raw[pos:pos + 2] == b"/*":
        end = raw.find(b"*/", pos + 2)
        return len(raw) if end < 0 else end + 2
    if char == 47 and (previous in REGEX_PREFIX_WORDS or previous in
                       (b"", b"=", b"(", b"[", b"{", b",", b":", b";", b"!", b"?", b"&", b"|", b"+", b"-", b"*", b"%", b"^", b"~", b"<", b">", b"=>")):
        return regex_end(raw, pos)
    return None


def lexical_mask(raw):
    masked, previous, pos = bytearray(raw), b"", 0
    while pos < len(raw):
        token = TOKEN.match(raw, pos)
        require(token is not None, "LEXER_PROGRESS")
        value, end = token.group(), token.end()
        if value.isspace():
            pos = end
            continue
        skipped = skip_literal(raw, pos, previous)
        if skipped is not None:
            is_comment = raw[pos:pos + 2] in (b"//", b"/*")
            masked[pos:skipped] = b" " * (skipped - pos)
            pos = skipped
            if not is_comment:
                previous = b"literal"
            continue
        previous, pos = value, end
    return bytes(masked)


def scopes(raw):
    mask = lexical_mask(raw)
    stack, pairs, parents, warnings = [], {}, {}, []
    closing = {41: 40, 93: 91, 125: 123}
    for token in re.finditer(rb"[()\[\]{}]", mask):
        char, offset = token.group()[0], token.start()
        if char in (40, 91, 123):
            parents[offset] = stack[-1][1] if stack else None
            stack.append((char, offset))
        elif stack and stack[-1][0] == closing[char]:
            _, opening = stack.pop()
            pairs[opening] = offset
        else:
            if len(warnings) < 12:
                warnings.append({"kind": "unpaired_delimiter", "byte_offset": offset})
    if stack:
        warnings.append({"kind": "unclosed_delimiters", "count": len(stack)})
    classes, functions = [], []
    pattern = rb"\bclass(?:\s+(?!extends\b)(" + IDENT + rb"))?(?:\s+extends\s+[^{};]{1,200})?\s*\{"
    for match in re.finditer(pattern, mask):
        opening = match.end() - 1
        if opening not in pairs:
            continue
        before = mask[max(0, match.start() - 100):match.start()]
        assignment = re.search(rb"(" + IDENT + rb")\s*=\s*$", before)
        symbol = assignment.group(1) if assignment else match.group(1)
        entry = {"symbol": symbol.decode() if symbol else None,
                 "inner_class_name": match.group(1).decode() if match.group(1) else None,
                 "start": match.start(), "body_start": opening,
                 "end": pairs[opening] + 1, "methods": []}
        classes.append(entry)
    classes_by_open = {entry["body_start"]: entry for entry in classes}
    method_pattern = rb"(?<![A-Za-z0-9_$])(#?" + IDENT + rb")\s*\("
    for match in re.finditer(method_pattern, mask):
        opening = match.end() - 1
        entry = classes_by_open.get(parents.get(opening))
        if entry is None or opening not in pairs:
            continue
        after = re.match(rb"\s*\{", mask[pairs[opening] + 1:pairs[opening] + 128])
        if after is None:
            continue
        body = pairs[opening] + 1 + after.end() - 1
        if body not in pairs:
            continue
        entry["methods"].append({"name": match.group(1).decode(), "start": match.start(),
                                 "body_start": body, "end": pairs[body] + 1})
    for match in re.finditer(rb"\bfunction\s+\*?\s*(" + IDENT + rb")\s*\(", mask):
        opening = match.end() - 1
        if opening not in pairs:
            continue
        after = re.match(rb"\s*\{", mask[pairs[opening] + 1:pairs[opening] + 128])
        if after is None:
            continue
        body = pairs[opening] + 1 + after.end() - 1
        if body in pairs:
            functions.append({"symbol": match.group(1).decode(), "start": match.start(),
                              "body_start": body, "end": pairs[body] + 1})
    return {"classes": classes, "functions": functions, "warnings": warnings}


def context(raw, point, match_len=1):
    start = max(0, point - 220)
    end = min(len(raw), start + CONTEXT_BYTES)
    # Align boundaries to UTF-8 codepoints; SHA always covers exact raw bytes.
    while start < len(raw) and raw[start] & 0xC0 == 0x80:
        start += 1
    while end < len(raw) and raw[end] & 0xC0 == 0x80:
        end -= 1
    selected = raw[start:end]
    return {"byte_start": start, "byte_end_exclusive": end,
            "match_byte_start": point, "match_byte_end_exclusive": point + match_len,
            "sha256": sha(selected), "text": selected.decode("utf-8", errors="strict")}


def occurrences(raw, needle, start=0, end=None):
    end = len(raw) if end is None else end
    positions, count, offset = [], 0, start
    while True:
        offset = raw.find(needle, offset, end)
        if offset < 0:
            return count, positions
        count += 1
        if len(positions) < 3:
            positions.append(offset)
        offset += max(1, len(needle))


def candidate_record(kind, entry, raw, matched):
    return {"symbol": entry["symbol"], "kind": kind,
            "status": "DIAGNOSTIC_CANDIDATE_REQUIRES_REVIEW",
            "scope_byte_start": entry["start"], "scope_byte_end_exclusive": entry["end"],
            "scope_sha256": sha(raw[entry["start"]:entry["end"]]),
            "matched_stable_definitions": matched,
            "methods": entry.get("methods", []),
            "declaration_context": context(raw, entry["start"])}


def run():
    extraction = json.loads(read_small(BASE / "receipts/target-extraction.json", 1_000_000))
    require(extraction.get("TARGET_BUILD_BYTES") == "PASS" and extraction.get("status") == "PASS", "EXTRACTION_GATE_NOT_PASS")
    sources, bundles, indexes = {}, {}, {}
    for name, (relative, expected) in TARGETS.items():
        raw = read_small(BASE / "target" / relative, MAX_INPUT)
        require(sha(raw) == expected, "BUNDLE_HASH_MISMATCH:" + name)
        raw.decode("utf-8", errors="strict")
        sources[name] = {"path": relative, "bytes": len(raw), "sha256": sha(raw)}
        bundles[name], indexes[name] = raw, scopes(raw)
    anchors_raw = read_small(BASE / "ANCHORS.json", 100_000)
    anchors = json.loads(anchors_raw)
    require(isinstance(anchors, list) and len(anchors) == 22, "ANCHOR_COUNT")
    require([a.get("id") for a in anchors] == list(range(1, 23)), "ANCHOR_IDS")
    equivalents, role_entries = {}, {}
    for role, (source, definitions, minimum) in ROLE_SPECS.items():
        found = []
        for entry in indexes[source]["classes"]:
            names = {m["name"] for m in entry["methods"]}
            matched = sorted(names.intersection(definitions))
            if len(matched) >= minimum:
                found.append((entry, matched))
        role_entries[role] = [entry for entry, _ in found[:3]]
        equivalents[role] = {"value": "NC", "status": "REVIEW_REQUIRED" if found else "NO_CANDIDATE",
                             "source": source, "candidate_count": len(found),
                             "candidates": [candidate_record("class", e, bundles[source], m) for e, m in found[:3]]}
    composer = []
    composer_properties = (b"aboveComposerHeaderContent", b"activeCollaborationMode", b"composerModeAvailability", b"surfacePlacement", b"isResponseInProgress")
    for entry in indexes["renderer"]["functions"]:
        header = bundles["renderer"][entry["start"]:entry["body_start"]]
        matched = [p.decode() for p in composer_properties if re.search(rb"(?<![A-Za-z0-9_$])" + p + rb"\s*:", header)]
        if len(matched) >= 3:
            composer.append((entry, matched))
    role_entries["COMPOSER_EQUIVALENT"] = [entry for entry, _ in composer[:3]]
    equivalents["COMPOSER_EQUIVALENT"] = {"value": "NC", "status": "REVIEW_REQUIRED" if composer else "NO_CANDIDATE",
        "source": "renderer", "candidate_count": len(composer),
        "candidates": [candidate_record("function", e, bundles["renderer"], m) for e, m in composer[:3]]}
    results, matched_count = [], 0
    raw = bundles["renderer"]
    for anchor in anchors:
        needle = anchor["old"].encode("utf-8")
        require(0 < len(needle) <= 10_000, "ANCHOR_TEXT_LENGTH")
        count, positions = occurrences(raw, needle)
        matched_count += count == 1
        role = ("SINK_CLASS_EQUIVALENT" if anchor["id"] <= 7 else
                "RUNTIME_CLASS_EQUIVALENT" if anchor["id"] <= 14 else
                "OWNER_CLASS_EQUIVALENT" if anchor["id"] <= 21 else "COMPOSER_EQUIVALENT")
        diagnostic = []
        selected_scopes = role_entries[role]
        for probe in PROBES[anchor["id"]]:
            pattern = probe.encode()
            hits = []
            for entry in selected_scopes:
                _, offsets = occurrences(raw, pattern, entry["start"], entry["end"])
                hits.extend(offsets[:2])
            # A missing scope does not establish absence; retain bounded clues.
            search_scope = "candidate_scope"
            if not selected_scopes:
                _, hits = occurrences(raw, pattern)
                search_scope = "whole_renderer_diagnostic_only"
            diagnostic.append({"probe": probe, "search_scope": search_scope,
                               "semantic_match": "NC", "contexts": [context(raw, p, len(pattern)) for p in hits[:2]]})
        results.append({"id": anchor["id"], "label": anchor["label"],
                        "old_text_sha256": sha(needle), "literal_occurrences": count,
                        "literal_status": "UNIQUE_EXACT_LITERAL" if count == 1 else "ABSENT" if count == 0 else "AMBIGUOUS_MULTIPLE_LITERALS",
                        "target_structural_equivalence": "NC", "candidate_role": role,
                        "literal_contexts": [context(raw, p, len(needle)) for p in positions[:1]],
                        "structural_diagnostics": diagnostic})
    common = {"schema_version": 1, "scope": "P0_STATIC_ONLY", "source_hashes": sources,
              "analyzer_sha256": sha(Path(__file__).read_bytes()), "anchors_sha256": sha(anchors_raw),
              "byte_offsets": "zero_based_utf8_bytes_end_exclusive",
              "analysis_kind": "diagnostic_lexical_scopes_not_javascript_AST",
              "lexical_warnings": {key: value["warnings"] for key, value in indexes.items()},
              "TARGET_BUILD_BYTES": "PASS", "EXACT_BUILD_MATCH": "NC",
              "OWNER_PATCH": "FROZEN_SOL_PASS", "P1": "NO", "P2": "NO"}
    contexts = {**common, "PATCH_ANCHORS_MATCHED": f"{matched_count}/22",
                "PATCH_ANCHORS_MATCHED_BASIS": "unique_exact_original_literal_only_not_structural_equivalence",
                "STRUCTURAL_ANCHORS_CONFIRMED": "NC/22", "anchors": results}
    classes = {**common, "SIX_EQUIVALENTS": equivalents,
               "remaining_nc": ["All structural anchor conditions require byte-context review", "All six class/component equivalences require review", "No patch applied or executed"]}
    outputs = {"TARGET-ANCHOR-CONTEXTS.json": contexts, "TARGET-CLASS-EQUIVALENTS.json": classes}
    encoded = {name: (json.dumps(value, ensure_ascii=False, separators=(",", ":")) + "\n").encode() for name, value in outputs.items()}
    require(sum(map(len, encoded.values())) <= MAX_OUTPUT, "RECEIPTS_TOTAL_SIZE_CAP")
    for name, data in encoded.items():
        path = BASE / "receipts" / name
        require(not path.exists() and not path.is_symlink(), "RECEIPT_ALREADY_EXISTS")
        with path.open("xb") as target:
            target.write(data)
    print(json.dumps({"status": "DIAGNOSTICS_COMPLETE", "PATCH_ANCHORS_MATCHED": f"{matched_count}/22",
                      "EXACT_BUILD_MATCH": "NC", "receipt_bytes": sum(map(len, encoded.values()))}))


if __name__ == "__main__":
    try:
        run()
    except (OSError, ValueError, KeyError, TypeError, RecursionError) as exc:
        print(json.dumps({"status": "STOP", "error": str(exc), "EXACT_BUILD_MATCH": "NC"}), file=sys.stderr)
        raise SystemExit(1)
