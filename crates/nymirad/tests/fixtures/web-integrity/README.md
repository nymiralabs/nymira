A plain copy of `web/test-fixtures/integrity/` from the web hosting work (`web/INTEGRITY.md` is
the contract). It is a small signed site made with a throwaway key. To refresh it, copy that
directory over this one again (`regenerate.sh` there makes a new key and rebuilds `site/`); the
expected values in `expected.json` and the tests in `tests/web_verify.rs` follow it.
