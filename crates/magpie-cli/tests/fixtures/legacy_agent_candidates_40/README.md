# Original observation 40

`manifest.json` and `cases.tsv` describe all 40 `agents`/`ls` observations,
including the eight byte-identical untraced controls. Files named by SHA-256
contain the complete original stdout/stderr bytes. Shared bytes use one file;
do not trim spaces, substitute Unicode, or remove final newlines.

Recreate every generated file using only Python's standard library:

```sh
python3 crates/magpie-cli/tests/fixtures/legacy_agent_candidates_40/extract.py /path/to/40-original-cli-pairs-default-home.md
```

The extractor requires the original evidence document's exact SHA-256 and
checks every stream length/hash. Rust tests use the same generated case table.
Pure rendering compares every byte unchanged. Process tests replace only the
explicit `/fixture/codex/config.toml` path with their private fixture path.
Default-home bytes, including `~/.codex/config.toml`, are never substituted.

Evidence covers pairs with the Codex directory absent. The default empty
directory was observed only without candidates, with Codex alone, and with all
three. Other directory combinations and actual configurations keep the prior
display path; these fixtures make no claim about their original behavior.
