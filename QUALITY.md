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

`scripts/coverage.sh` applies three independent policies:

1. Total Rust line coverage may not fall below 54.8%, the measured
   whole-program baseline when this policy was last ratcheted.
2. Every line in the deterministic contract layer must be covered. This is the
   protocol crate, the durable voice-outbox state machine, and the exhaustive
   upstream-event policy in `event_coverage.rs`; the threshold is both 100%
   overall and 100% per file.
3. In CI, every executable Rust line added or changed relative to the pull
   request base (or previous pushed commit) must be covered. Diff coverage is
   therefore 100%, preventing new coverage debt while legacy integration
   boundaries remain visible in the whole-program report.

The following executable or integration-boundary files are deliberately outside
the 100% contract metric and remain visible in the whole-program report:

| Boundary | Verification |
| --- | --- |
| `daemon/main.rs` | Rust tests, release build, isolated daemon/IPC smoke test, CodeQL |
| `daemon/database.rs` | SQLite unit tests, smoke test, whole-program coverage floor |
| `daemon/assets.rs` | Filesystem/media unit tests, whole-program coverage floor |
| `daemon/notification.rs` | Message-rendering unit tests, whole-program coverage floor |
| `ctl/main.rs` | Launcher-generation unit tests and daemon/IPC smoke test |
| Quickshell QML/JS | Qt Quick unit/component/state/workflow tests, 100% portable behavioral-contract coverage, 100% semantic mutation score, `qmlformat`, strict local `qmllint`, and Omarchy plugin validation locally |
| Installer/service/package | ShellCheck, version and metadata checks, locked release build, packaged plugin discovery test, isolated install/update/uninstall lifecycle test, live install verification |

This boundary is explicit because reporting 100% by silently excluding
uncovered application code would be misleading. Contract and diff coverage
guarantee that stable and newly changed testable code is fully exercised, while
the total threshold keeps legacy integration code visible and can only ratchet
upward. Raise the total baseline whenever new tests improve it.

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
