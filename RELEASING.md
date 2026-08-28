# Releasing

Releases use semantic versions and are cut from a reviewed, green `main`.

1. Update the workspace version in `Cargo.toml`, the Quickshell manifest, and
   `PKGBUILD` in one pull request.
2. Run `./scripts/generate-license-report.sh` so the generated project version
   and dependency inventory are current.
3. Run every command in [`QUALITY.md`](QUALITY.md), then install and exercise
   the result on an Omarchy workstation without clearing account state.
4. Complete the Omarchy lifecycle checklist:
   - validate both the repository root and installed plugin directory;
   - open from the bar, close transient menus with Escape, then close the app
     through its header action and test shell summon and hide;
   - disable and re-enable the plugin, confirming its service and panel reload;
   - restart Omarchy Shell and confirm recent logs contain no QML load errors;
   - uninstall without `--purge-data`, confirm the account state remains, then
     reinstall and confirm the daemon reconnects without relinking.
   Restore the previous bar position if the disable/re-enable test changes it.
5. Merge only after the required GitHub checks and review pass.
6. Create a signed `vX.Y.Z` tag whose version matches the package metadata and
   points to a commit already present on protected `main`, then push that tag.
   CI reruns all quality, coverage, packaging, and smoke gates for release tags.
7. The release workflow verifies the tag and version, produces a reproducible
   x86_64 archive, publishes its SHA-256 checksum, records a GitHub/Sigstore
   provenance attestation, and creates the GitHub release. Add release notes
   describing user-visible changes, upgrade impact, security fixes, and known
   protocol limitations.

Keep the previous release available so users can roll back binaries while
preserving `~/.local/state/omarchy-whatsapp`. Verify downloaded archives with
both the published checksum and `gh attestation verify`. Never package or
publish the local state directory.
