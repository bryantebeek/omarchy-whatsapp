# Quality policy

The repository has one local entry point for its required checks:

```bash
./scripts/check.sh
./scripts/coverage.sh
./scripts/cargo.sh deny check
./scripts/cargo.sh build --release --locked --workspace
./tests/smoke.sh
```

GitHub Actions runs the same gates for every pull request and for `main`.
Merging should require the `Quality`, `Coverage`, `Smoke`, `Dependency review`,
`Supply chain`, and `CodeQL` checks through the branch protection rules in the
GitHub repository settings.

## One-time GitHub settings

Configuration files cannot enable repository rules themselves. After these
workflows first reach GitHub, create a ruleset for `main` that:

- requires a pull request, resolved conversations, and all six checks named
  above;
- requires the branch to be up to date before merge;
- blocks force pushes and deletion and requires linear history;
- limits the default `GITHUB_TOKEN` to read-only and permits write access only
  in a future, narrowly scoped release workflow;
- enables private vulnerability reporting, Dependabot alerts/security updates,
  secret scanning, and push protection where the repository plan supports them.

Also protect `v*` tags from updates or deletion. Repository administrators
should not bypass these rules for routine changes.

This user-owned repository currently has one maintainer, so its active ruleset
requires zero approvals; requiring one would make the maintainer's own pull
requests impossible to merge. Raise the requirement to one and enable stale
approval dismissal when a second maintainer with write access is added.

## Coverage contract

`scripts/coverage.sh` applies two independent policies:

1. Total Rust line coverage may not fall below 54%, the measured whole-program
   baseline when this policy was introduced.
2. Every line in the deterministic contract layer must be covered. This is the
   protocol crate and the exhaustive upstream-event policy in
   `event_coverage.rs`; the threshold is both 100% overall and 100% per file.

The following executable or integration-boundary files are deliberately outside
the 100% contract metric and remain visible in the whole-program report:

| Boundary | Verification |
| --- | --- |
| `daemon/main.rs` | Rust tests, release build, isolated daemon/IPC smoke test, CodeQL |
| `daemon/database.rs` | SQLite unit tests, smoke test, whole-program coverage floor |
| `daemon/assets.rs` | Filesystem/media unit tests, whole-program coverage floor |
| `daemon/notification.rs` | Message-rendering unit tests, whole-program coverage floor |
| `ctl/main.rs` | Launcher-generation unit tests and daemon/IPC smoke test |
| Quickshell QML/JS | `qmlformat`, strict local `qmllint`, portable manifest checks, Omarchy plugin validation locally |
| Installer/service/package | ShellCheck, version and metadata checks, locked release build, live install verification |

This boundary is explicit because reporting 100% by silently excluding
uncovered application code would be misleading. The contract threshold catches
every uncovered line in the most stable, security-sensitive serialization
surface, while the total threshold prevents untested application code from
growing unnoticed. Raise the total baseline whenever new tests improve it.

Coverage is evidence that lines executed, not proof that every behavior is
correct. Tests should still cover success, failure, boundary, and regression
cases rather than executing lines solely to increase the number.

## Pull-request expectations

- Keep changes focused and update tests with behavior changes.
- Never use real account databases, pairing QR codes, credentials, private
  phone numbers, or captured message payloads as fixtures.
- Keep `Cargo.lock` committed and use `--locked` in CI and packaging.
- Update `Cargo.toml`, the Quickshell manifest, `PKGBUILD`, and the generated
  license report together for a release version change.
- Run the local gates before requesting review. Rust or packaging changes must
  also be installed and verified according to `AGENTS.md` on an Omarchy host.
