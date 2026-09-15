#!/usr/bin/env python3
"""P0-only extraction of two pinned bundles from a verified local MSIX copy.

No network, installed-package access, code execution, patching, or general
extraction. All output paths are fixed beneath this script's directory.
Existing outputs cause STOP; incomplete outputs are retained as evidence.
"""

import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import stat
import struct
import sys
from datetime import datetime, timezone
import xml.etree.ElementTree as ET
import zipfile


BASE = Path(__file__).absolute().parent
VERSION = "26.908.4834.0"
ARCHITECTURE = "x64"
TARGETS = {
    "webview/assets/app-initial-d9bed9d614d8.js": (
        "renderer_sha256",
        "7c3a89e7e224f76031b45a88f72af8cd60f0c3d47aac9ca34b2c70e11dfe9867",
    ),
    ".vite/build/main-D8abTQQE.js": (
        "main_sha256",
        "b55be874a9b5a262c09a7945df38cec9b0ce8f14bd584ef73d6feca301ed90b4",
    ),
}
ASAR_NAME = "app/resources/app.asar"
MANIFEST_NAME = "AppxManifest.xml"
MAX_ASAR = 2 * 1024**3
MAX_HEADER = 32 * 1024**2
MAX_JS = 64 * 1024**2
MAX_MANIFEST = 1024**2
CHUNK = 1024**2


class Stop(ValueError):
    pass


def require(condition, code):
    if not condition:
        raise Stop(code)


def unique_object(pairs):
    result = {}
    for key, value in pairs:
        require(key not in result, "DUPLICATE_JSON_KEY")
        result[key] = value
    return result


def load_json(data):
    def reject_constant(_):
        raise Stop("NONFINITE_JSON_NUMBER")
    return json.loads(data, object_pairs_hook=unique_object,
                      parse_constant=reject_constant)


def safe_existing_path(path, directory=False):
    path = Path(os.path.abspath(path))
    for ancestor in reversed((path, *path.parents)):
        info = ancestor.lstat()
        require(not stat.S_ISLNK(info.st_mode), "FILESYSTEM_SYMLINK")
        if ancestor != path or directory:
            require(stat.S_ISDIR(info.st_mode), "NOT_A_DIRECTORY")
    if not directory:
        require(stat.S_ISREG(path.lstat().st_mode), "NOT_A_REGULAR_FILE")
    return path


def open_read(path):
    path = safe_existing_path(path)
    fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW)
    stream = os.fdopen(fd, "rb")
    require(stat.S_ISREG(os.fstat(stream.fileno()).st_mode), "NOT_A_REGULAR_FILE")
    return stream


def output_path(relative):
    parts = relative.split("/")
    require(all(part and part not in (".", "..") for part in parts),
            "UNSAFE_OUTPUT_PATH")
    safe_existing_path(BASE, directory=True)
    parent = BASE
    for part in parts[:-1]:
        parent = parent / part
        try:
            parent.mkdir(mode=0o700)
        except FileExistsError:
            pass
        safe_existing_path(parent, directory=True)
    return parent / parts[-1]


def create_new(path):
    safe_existing_path(path.parent, directory=True)
    fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW,
                 0o600)
    return os.fdopen(fd, "wb")


def write_new(path, data):
    with create_new(path) as stream:
        stream.write(data)
        stream.flush()
        os.fsync(stream.fileno())


def stat_stamp(stream):
    info = os.fstat(stream.fileno())
    return (info.st_dev, info.st_ino, info.st_size,
            info.st_mtime_ns, info.st_ctime_ns)


def read_exact(stream, size):
    data = stream.read(size)
    require(len(data) == size, "TRUNCATED_READ")
    return data


def hash_msix(stream):
    sha1, sha256 = hashlib.sha1(), hashlib.sha256()
    size = 0
    while chunk := stream.read(CHUNK):
        sha1.update(chunk)
        sha256.update(chunk)
        size += len(chunk)
        require(size <= 2 * 1024**3, "MSIX_SIZE_CAP")
    stream.seek(0)
    return {"bytes": size, "sha1": sha1.hexdigest(),
            "sha256": sha256.hexdigest()}


def check_zip(archive):
    entries = archive.infolist()
    require(len(entries) <= 100_000, "ZIP_ENTRY_CAP")
    by_name, folded = {}, set()
    for entry in entries:
        name = entry.filename
        require(name == entry.orig_filename and "\0" not in name,
                "ZIP_TRUNCATED_FILENAME")
        require(not name.startswith("/") and "\\" not in name and ":" not in name,
                "ZIP_UNSAFE_PATH")
        trimmed = name[:-1] if entry.is_dir() else name
        parts = trimmed.split("/")
        require(all(part and part not in (".", "..") for part in parts),
                "ZIP_PATH_TRAVERSAL")
        require(trimmed.casefold() not in folded, "ZIP_DUPLICATE_PATH")
        folded.add(trimmed.casefold())
        mode = entry.external_attr >> 16
        kind = stat.S_IFMT(mode)
        require(kind in (0, stat.S_IFREG, stat.S_IFDIR), "ZIP_SPECIAL_FILE")
        require(not (entry.external_attr & 0x400), "ZIP_REPARSE_POINT")
        require(not (entry.flag_bits & 1), "ZIP_ENCRYPTED")
        require(entry.file_size >= 0 and entry.compress_size >= 0,
                "ZIP_INVALID_SIZE")
        by_name[trimmed] = entry
    folded_entries = {name.casefold(): entry for name, entry in by_name.items()}
    for name in by_name:
        for parent in Path(name).parents:
            if str(parent) == ".":
                continue
            entry = folded_entries.get(str(parent).casefold())
            require(entry is None or entry.is_dir(), "ZIP_FILE_PARENT_COLLISION")
    for name, cap in ((ASAR_NAME, MAX_ASAR), (MANIFEST_NAME, MAX_MANIFEST)):
        require(name in by_name, "ZIP_MISSING_" + name)
        entry = by_name[name]
        require(not entry.is_dir() and 0 < entry.file_size <= cap,
                "ZIP_SELECTED_ENTRY_SIZE")
    return by_name


def extract_zip_entry(archive, entry, destination):
    digest = hashlib.sha256()
    written = 0
    with archive.open(entry, "r") as source, create_new(destination) as output:
        while chunk := source.read(CHUNK):
            written += len(chunk)
            require(written <= entry.file_size, "ZIP_EXCESS_BYTES")
            digest.update(chunk)
            output.write(chunk)
        require(written == entry.file_size, "ZIP_TRUNCATED_ENTRY")
        output.flush()
        os.fsync(output.fileno())
    return {"path": str(destination), "archive_path": entry.filename,
            "bytes": written, "sha256": digest.hexdigest()}


def manifest_identity(path):
    with open_read(path) as source:
        raw = source.read(MAX_MANIFEST + 1)
    require(len(raw) <= MAX_MANIFEST, "MANIFEST_SIZE")
    text = raw.decode("utf-8-sig", errors="strict")
    require("<!DOCTYPE" not in text.upper() and "<!ENTITY" not in text.upper(),
            "XML_DECLARATION_REJECTED")
    root = ET.fromstring(text)
    ns = "{http://schemas.microsoft.com/appx/manifest/foundation/windows10}"
    require(root.tag == ns + "Package", "MANIFEST_ROOT")
    identities = root.findall(ns + "Identity")
    require(len(identities) == 1, "MANIFEST_IDENTITY_COUNT")
    result = dict(identities[0].attrib)
    require(result.get("Version") == VERSION, "MANIFEST_VERSION_MISMATCH")
    require(result.get("ProcessorArchitecture") == ARCHITECTURE,
            "MANIFEST_ARCHITECTURE_MISMATCH")
    require(result.get("Name") == "OpenAI.Codex", "MANIFEST_NAME_MISMATCH")
    return result


def asar_index(stream, archive_size):
    first_payload, header_size = struct.unpack("<II", read_exact(stream, 8))
    require(first_payload == 4, "ASAR_PICKLE_LAYOUT")
    require(8 <= header_size <= MAX_HEADER and header_size <= archive_size - 8,
            "ASAR_HEADER_BOUNDS")
    encoded = read_exact(stream, header_size)
    payload_size, json_size = struct.unpack_from("<Ii", encoded)
    require(json_size >= 0, "ASAR_NEGATIVE_JSON_SIZE")
    require(payload_size == 4 + ((json_size + 3) // 4) * 4
            and header_size == 4 + payload_size
            and json_size <= header_size - 8, "ASAR_PICKLE_LENGTHS")
    require(not any(encoded[8 + json_size:]), "ASAR_NONZERO_PADDING")
    header = load_json(encoded[8:8 + json_size].decode("utf-8", errors="strict"))
    require(isinstance(header, dict) and isinstance(header.get("files"), dict),
            "ASAR_ROOT")
    base = 8 + header_size
    payload_size = archive_size - base
    selected, ranges = {}, []
    work = [("", header, 0)]
    node_count = 0
    while work:
        name, node, depth = work.pop()
        node_count += 1
        require(node_count <= 1_000_000 and depth <= 64, "ASAR_TREE_CAP")
        require(isinstance(node, dict), "ASAR_INVALID_NODE")
        require("link" not in node, "ASAR_SYMLINK_REJECTED")
        if "unpacked" in node:
            require(type(node["unpacked"]) is bool, "ASAR_INVALID_UNPACKED")
        if "files" in node:
            require(isinstance(node["files"], dict) and "offset" not in node
                    and "size" not in node, "ASAR_INVALID_DIRECTORY")
            names = set()
            for child, value in node["files"].items():
                require(child and child not in (".", "..")
                        and not any(c in child for c in "/\\\0:"),
                        "ASAR_PATH_TRAVERSAL")
                require(child.casefold() not in names, "ASAR_DUPLICATE_PATH")
                names.add(child.casefold())
                work.append(((name + "/" if name else "") + child, value, depth + 1))
            continue
        size = node.get("size")
        require(type(size) is int and 0 <= size <= MAX_ASAR, "ASAR_FILE_SIZE")
        if node.get("unpacked") is True:
            require(name not in TARGETS, "ASAR_TARGET_UNPACKED")
            continue
        offset_text = node.get("offset")
        require(isinstance(offset_text, str) and len(offset_text) <= 20
                and re.fullmatch(r"[0-9]+", offset_text), "ASAR_OFFSET_SYNTAX")
        offset = int(offset_text)
        require(offset <= payload_size and size <= payload_size - offset,
                "ASAR_ENTRY_BOUNDS")
        if size:
            ranges.append((offset, offset + size))
        if name in TARGETS:
            require(0 < size <= MAX_JS, "ASAR_TARGET_SIZE")
            selected[name] = (base + offset, size)
    ranges.sort()
    previous_end = 0
    for start, end in ranges:
        require(start >= previous_end, "ASAR_OVERLAPPING_FILES")
        previous_end = end
    require(set(selected) == set(TARGETS), "ASAR_MISSING_TARGET")
    return selected, {"header_bytes": header_size, "data_offset": base,
                      "header_sha256": hashlib.sha256(encoded).hexdigest(),
                      "nodes_validated": node_count}


def run(msix_path, expected_path, receipt):
    with open_read(expected_path) as source:
        expected_raw = source.read(1024**2 + 1)
    require(len(expected_raw) <= 1024**2, "EXPECTED_SOURCES_SIZE")
    expected = load_json(expected_raw.decode("utf-8"))
    require(isinstance(expected, dict), "EXPECTED_SOURCES_ROOT")
    require(expected.get("version") == VERSION
            and expected.get("architecture") == ARCHITECTURE, "EXPECTED_TARGET")
    for key, count in (("sha1", 40), ("sha256", 64)):
        require(isinstance(expected.get(key), str)
                and re.fullmatch(r"[0-9a-f]{%d}" % count, expected[key]),
                "EXPECTED_DIGEST_FORMAT")
    require(type(expected.get("expected_bytes")) is int
            and 0 < expected["expected_bytes"] <= 2 * 1024**3,
            "EXPECTED_MSIX_SIZE")
    for key, pinned in TARGETS.values():
        require(expected.get(key) == pinned, "EXPECTED_BUNDLE_DIGEST_MISMATCH")
    receipt["expected_sources"] = {
        "path": str(expected_path),
        "sha256": hashlib.sha256(expected_raw).hexdigest(),
        "values": expected,
    }
    destinations = {name: output_path("work/msix/" + name)
                    for name in (MANIFEST_NAME, ASAR_NAME)}
    target_destinations = {name: output_path("target/" + name) for name in TARGETS}
    for destination in (*destinations.values(), *target_destinations.values()):
        require(not os.path.lexists(destination), "OUTPUT_ALREADY_EXISTS")
    with open_read(msix_path) as msix:
        initial_stat = stat_stamp(msix)
        require(initial_stat[2] <= 2 * 1024**3, "MSIX_SIZE_CAP")
        measured = hash_msix(msix)
        receipt["msix"] = {"path": str(msix_path), **measured}
        require(stat_stamp(msix) == initial_stat, "MSIX_CHANGED_DURING_HASH")
        require(measured["bytes"] == expected["expected_bytes"], "MSIX_SIZE_MISMATCH")
        require(measured["sha1"] == expected["sha1"], "MSIX_SHA1_MISMATCH")
        require(measured["sha256"] == expected["sha256"], "MSIX_SHA256_MISMATCH")
        receipt["msix"]["digest_match"] = "PASS"
        with zipfile.ZipFile(msix, "r") as archive:
            entries = check_zip(archive)
            receipt["zip_entries_indexed"] = len(entries)
            receipt["manifest"] = extract_zip_entry(
                archive, entries[MANIFEST_NAME], destinations[MANIFEST_NAME])
            receipt["identity"] = manifest_identity(destinations[MANIFEST_NAME])
            receipt["asar"] = extract_zip_entry(
                archive, entries[ASAR_NAME], destinations[ASAR_NAME])
        require(stat_stamp(msix) == initial_stat, "MSIX_CHANGED_DURING_EXTRACTION")
    with open_read(destinations[ASAR_NAME]) as asar:
        asar_stat = stat_stamp(asar)
        selected, metadata = asar_index(asar, asar_stat[2])
        receipt["asar"]["index"] = metadata
        bundles, receipt["bundles"] = {}, {}
        for name, (offset, size) in selected.items():
            asar.seek(offset)
            raw = read_exact(asar, size)
            digest = hashlib.sha256(raw).hexdigest()
            receipt["bundles"][name] = {
                "path": str(target_destinations[name]), "bytes": size,
                "asar_absolute_offset": offset, "sha256": digest,
                "expected_sha256": TARGETS[name][1],
                "digest_match": "PASS" if digest == TARGETS[name][1] else "FAIL",
            }
            require(digest == TARGETS[name][1], "BUNDLE_SHA256_MISMATCH:" + name)
            bundles[name] = raw
        require(stat_stamp(asar) == asar_stat, "ASAR_CHANGED_DURING_EXTRACTION")
    # No JavaScript decoding or analysis occurs in this script. Both raw hashes
    # must pass before either selected bundle is persisted to its target path.
    for name, raw in bundles.items():
        write_new(target_destinations[name], raw)
    receipt["TARGET_BUILD_BYTES"] = "PASS"
    receipt["status"] = "PASS"


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--msix", required=True, type=Path)
    parser.add_argument("--expected", type=Path,
                        default=BASE / "evidence/EXPECTED-SOURCES.json")
    args = parser.parse_args()
    receipt = {"schema_version": 1, "status": "STOP", "TARGET_BUILD_BYTES": "NC",
               "started_utc": datetime.now(timezone.utc).isoformat(),
               "scope": "P0_STATIC_ONLY", "signature_verification": "NOT_PERFORMED",
               "P1": "NOT_STARTED", "P2": "NOT_STARTED"}
    receipt_path = None
    try:
        receipt_path = output_path("receipts/target-extraction.json")
        require(not os.path.lexists(receipt_path), "RECEIPT_ALREADY_EXISTS")
        run(Path(os.path.abspath(args.msix)), Path(os.path.abspath(args.expected)), receipt)
    except (Stop, OSError, ValueError, KeyError, TypeError, RecursionError,
            struct.error, zipfile.BadZipFile, NotImplementedError, ET.ParseError) as error:
        receipt["status"] = "STOP"
        receipt["TARGET_BUILD_BYTES"] = "NC"
        receipt["reason"] = type(error).__name__ + ": " + str(error)
    receipt["finished_utc"] = datetime.now(timezone.utc).isoformat()
    encoded = (json.dumps(receipt, indent=2, ensure_ascii=False) + "\n").encode("utf-8")
    if receipt_path is not None:
        try:
            write_new(receipt_path, encoded)
        except (OSError, Stop) as error:
            print("STOP: receipt not written: " + str(error), file=sys.stderr)
            print(encoded.decode("utf-8"), end="")
            return 2
    print(encoded.decode("utf-8"), end="")
    return 0 if receipt["status"] == "PASS" else 2


if __name__ == "__main__":
    raise SystemExit(main())
