# Original 0.1.1948 synthetic candidate bytes

These files are decoded directly from the complete Base64 records in
`39-original-cli-candidate-behavior.md`, collected on 2026-10-29 (Asia/Taipei).
`manifest.json` records the document/binary hashes, original argv, exact stream
hashes/lengths, return codes, and all 16 agents/ls observations. Byte-identical
empty-Codex-directory and untraced controls reuse the same golden files.

Do not trim trailing spaces or replace Unicode characters. CLI tests substitute
only `/fixture/codex/config.toml` with their explicitly supplied synthetic
CODEX_HOME path; renderer tests retain the original path. No real configuration,
account, executable, or network service was used to collect these bytes.

Only the observed single candidates and all-three candidate set are covered.
Pairwise sets and populated configuration states are not original goldens.
