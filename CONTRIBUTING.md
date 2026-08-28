# Contributing

Thank you for helping improve Omarchy WhatsApp. Please read
[`QUALITY.md`](QUALITY.md) before opening a pull request.

Use the pinned Rust toolchain and keep dependency resolution reproducible:

```bash
./scripts/check.sh
./scripts/coverage.sh
./scripts/cargo.sh deny check
./scripts/cargo.sh build --release --locked --workspace
./tests/smoke.sh
```

Do not include paired-account state or real conversations in issues, tests,
logs, screenshots, or commits. Use invented JIDs, names, messages, and media.
Report security issues privately as described in [`SECURITY.md`](SECURITY.md).

Commits should be small enough to review, explain why the behavior changes, and
include regression tests where practical. Pull requests must pass all required
GitHub checks and receive review before merge.

Maintainers should follow [`RELEASING.md`](RELEASING.md) for versioned releases.
