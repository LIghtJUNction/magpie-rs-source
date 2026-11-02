# Verification scope

Rust workspace tests exercise the current shared Provider store and HTTP gateway.
The desktop is an independent workspace; use its `visual-test` feature for Provider,
Gateway and input integration tests. `scripts/desktop-fixture.py` supplies a real
native window with isolated synthetic files for manual end-to-end verification.

The Python `provider_*.py` and `legacy_stream_boundary.py` files added by the
36086d0 checkpoint target the earlier environment-reference Provider prototype.
Their old store/CLI schema is not the current shared ProviderStore contract; run
those historical regressions on that checkpoint, or explicitly port the cases to
the current Rust integration tests. They are retained as historical fixtures,
not reported as passing checks for the integrated implementation.

Actual native input, pixel review, synthetic upstream behavior, real external
accounts and OS installation are distinct evidence. See the recipe coverage and
`docs/NEXT_SESSION.md` for remaining product requirements.
