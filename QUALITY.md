# Quality policy

The repository has one local entry point for its required checks:

```bash
./scripts/check.sh
./scripts/coverage.sh
./scripts/qml-coverage.sh
./scripts/qml-mutation.sh
./scripts/cargo.sh deny check
./scripts/cargo.sh build --release --locked --workspace
./tests/smoke.sh
./tests/deployment-lifecycle.sh
./tests/package-layout.sh
```

GitHub Actions runs the same gates for every pull request and for `main`.
Merging should require the `Quality`, `Coverage`, `Smoke`, `Dependency review`,
`Supply chain`, and `CodeQL` checks through the branch protection rules in the
GitHub repository settings.

The version gate checks both release metadata and the daemon/QML IPC protocol
constant. Changing the wire contract without updating the shell handshake fails
locally and in CI before a partially compatible build can be packaged.

## One-time GitHub settings

Keep the repository's `main` ruleset configured to:

- requires a pull request, resolved conversations, and the `Quality`,
  `Coverage`, `Smoke`, `Supply chain`, `Dependency review`, `CodeQL`, four
  `QML / …` suite checks, `QML / Coverage contract`, and `QML / Mutation`
  checks;
- requires the branch to be up to date before merge;
- blocks force pushes and deletion and requires linear history;
- requires CodeQL results at the configured security threshold in addition to
  requiring the workflow status;
- limits the default `GITHUB_TOKEN` to read-only and grants write access only
  to the narrowly scoped release job;
- requires full-length action SHAs and permits only GitHub-owned actions plus
  the explicitly approved third-party action repositories;
- enables private vulnerability reporting, Dependabot alerts/security updates,
  secret scanning, and push protection; enable non-provider pattern scanning
  and validity checks as well when GitHub makes them available to the
  repository.

Also protect `v*` tags from updates or deletion. Repository administrators
should not bypass these rules for routine changes.

This user-owned repository currently has one maintainer, so its active ruleset
requires zero approvals; requiring one would make the maintainer's own pull
requests impossible to merge. Raise the requirement to one and enable stale
approval dismissal when a second maintainer with write access is added.

## Coverage contract

`scripts/coverage.sh` requires 100% Rust source-line coverage across the
workspace. It runs the complete all-features test matrix under the pinned
nightly toolchain, exports LCOV, and fails with exact `file:line` diagnostics if
any executable `DA` source line has a zero execution count. In CI, the existing
diff check also requires every executable line added or changed relative to the
pull-request base (or previous pushed commit) to be covered.

The source-line definition is deliberate and reproducible. LLVM's aggregate
`LF`/`LH` counters also invent locations for macro expansions, generic
monomorphizations, and `?` continuations that have no LCOV `DA` mapping to a
Rust source line. Those synthetic locations are not developer-addressable and
are not part of the gate. Test-module bodies are excluded from the denominator;
their calls still execute the instrumented production code.

Production exclusions must remain narrow and explicit:

- `coverage(off)` is allowed only on process/bootstrap loops, filesystem or
  SQLite row-decoding adapters, and upstream SDK/network calls whose behavior
  terminates outside this process. Deterministic decoding, validation, identity,
  ordering, revision, cryptographic, and state-transition helpers stay measured.
- `daemon/src/live_location/transport.rs` is the sole filename exclusion. It is
  the `async_trait` adapter that acknowledges an encrypted stanza through a live
  WhatsApp client. The cryptographic parser and ratchet state machine remain in
  `live_location.rs` and are fully source-line covered.
- Broad crate, module, or application-file exclusions are forbidden.

SQLite migrations and the excluded live transport are still exercised by
dedicated integration or deployment smoke tests. Quickshell QML/JS separately
requires the four Qt Quick suites, 100% portable behavioral-contract coverage,
100% semantic mutation score, `qmlformat`, strict local `qmllint`, and Omarchy
plugin validation. Installer, service, and package behavior is checked by
ShellCheck, metadata validation, locked release builds, isolated lifecycle
smoke tests, and live installation verification.

Coverage is evidence that lines executed, not proof that every behavior is
correct. Tests should still cover success, failure, boundary, and regression
cases rather than executing lines solely to increase the number.

Qt's open-source Quick Test runner does not expose interpreted QML/JavaScript
line or branch coverage; Qt's QML source-coverage tooling is commercial. The
portable frontend metric therefore makes a narrower, auditable claim rather
than reporting fabricated line coverage: every public `Model.js` helper, every
handled `Service.qml` event, every plugin entry point, and every required UI
workflow must have a Qt Quick test. `scripts/qml-mutation.sh` complements this
contract by applying semantic mutants to the actual `Model.js`, `Service.qml`,
and `Panel.qml` sources and requiring the tests to kill 100% of them.

## Pull-request expectations

- Keep changes focused and update tests with behavior changes.
- Never use real account databases, pairing QR codes, credentials, private
  phone numbers, or captured message payloads as fixtures.
- Keep `Cargo.lock` committed and use `--locked` in CI and packaging.
- Update `Cargo.toml`, the Quickshell manifest, `PKGBUILD`, and the generated
  license report together for a release version change.
- Run the local gates before requesting review. Rust or packaging changes must
  also be installed and verified according to `AGENTS.md` on an Omarchy host.
