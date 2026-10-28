#!/usr/bin/python3
"""Recreate the byte fixtures from the immutable original observation 40."""
import argparse
import base64
import hashlib
import json
from pathlib import Path
import re

DOCUMENT_SHA256 = "bfe8ae99a89ab53cedb6f2f8734e9485d6d0bf2d177d6c981263ce18408a0afa"
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("evidence", type=Path)
args = parser.parse_args()
document = args.evidence.read_bytes()
assert hashlib.sha256(document).hexdigest() == DOCUMENT_SHA256
records = json.loads(re.search(rb"```json\n(.*?)\n```", document, re.S).group(1))
destination = Path(__file__).resolve().parent
cases = ["run\tmode\tdirectory\tprograms\tcommand\texit\tstdout\tstderr"]
manifest = []
for record in records:
    if record["argv"] not in [["/usr/bin/magpie", "agents"], ["/usr/bin/magpie", "ls"]]:
        continue
    entry = {
        key: record[key]
        for key in [
            "run_number", "scenario", "argv", "exit_code", "codex_home_mode",
            "fake_programs", "tracing_enabled", "candidate_state_before",
        ]
    }
    for stream in ["stdout", "stderr"]:
        data = base64.b64decode(record[stream + "_base64"], validate=True)
        digest = hashlib.sha256(data).hexdigest()
        assert digest == record[stream + "_sha256"]
        assert len(data) == record["output_bytes"][stream]
        filename = digest + ".bin"
        (destination / filename).write_bytes(data)
        entry[stream] = {"file": filename, "sha256": digest, "bytes": len(data)}
    cases.append("\t".join([
        str(entry["run_number"]),
        entry["codex_home_mode"],
        str(int(entry["candidate_state_before"]["codex_directory_exists"])),
        ",".join(entry["fake_programs"]) or "-",
        entry["argv"][1],
        str(entry["exit_code"]),
        entry["stdout"]["file"],
        entry["stderr"]["file"],
    ]))
    manifest.append(entry)
assert len(manifest) == 40
(destination / "cases.tsv").write_text("\n".join(cases) + "\n")
(destination / "manifest.json").write_text(json.dumps({
    "evidence_document": args.evidence.name,
    "evidence_document_sha256": DOCUMENT_SHA256,
    "original_binary_sha256": "9aa760cbac67f375a810fcf15b9adf04edcb2cc7de64a9ebb0963da15c1d3eb7",
    "original_version": "0.1.1948",
    "records": manifest,
}, indent=2) + "\n")
print(f"extracted {len(manifest)} observations, {len(list(destination.glob('*.bin')))} unique streams")
