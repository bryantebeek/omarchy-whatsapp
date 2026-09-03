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

## The `whatsapp-rust` pin

The daemon depends on a `whatsapp-rust` commit that upstream has not merged
yet: the head of [oxidezap/whatsapp-rust#1397](https://github.com/oxidezap/whatsapp-rust/pull/1397),
which adds the manual presence policy the daemon needs to keep the linked
device from advertising itself as available on every reconnect. There is no
fork; the commit resolves through the pull-request ref of the upstream
repository. Wait for upstream to merge that pull request, then move the pin to
the merged commit or the next tagged release. Do not bump the dependency for
any other reason in the meantime, and never point it at a fork again. The
comment above the dependency in `Cargo.toml` lists what a bump has to
re-verify (the presence policy, the `transport.rs` adapter, and the private
`session.db` schema the resync code touches).

Commits should be small enough to review, explain why the behavior changes, and
include regression tests where practical. Pull requests must pass all required
GitHub checks and receive review before merge.

Maintainers should follow [`RELEASING.md`](RELEASING.md) for versioned releases.
