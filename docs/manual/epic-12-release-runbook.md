# EPIC-12 — Release & CI Verification Runbook

## Release Heading & Changelog Contract

`CHANGELOG.md` uses exact Keep-a-Changelog headings (`## [X.Y.Z] - YYYY-MM-DD`) aligned with `.github/workflows/release.yml` (`v*.*.*` tag pushes). When cutting a release for `v0.1.0`, the matching section heading in `CHANGELOG.md` is `## [0.1.0] - YYYY-MM-DD`.

*(Note: Homebrew tap formulas and AUR PKGBUILDs are intentionally out of scope for `0.1.0` and tracked as future distribution work.)*

## Tag-to-Release Steps

1. Confirm working tree is clean on the release commit and `CHANGELOG.md` contains a dated `## [X.Y.Z]` section moved out of `## [Unreleased]`.
2. Ensure `Cargo.toml` `version` matches `X.Y.Z` and run all verification gates locally:
   ```sh
   cargo fmt --all -- --check
   cargo clippy --all-targets -- -D warnings
   cargo test --all
   cargo test --release --test perf_gates -- --ignored --nocapture
   cargo package --allow-dirty --no-verify
   ```
3. Run a dry-run build via GitHub Actions `workflow_dispatch` on `.github/workflows/release.yml` if validating artifact packaging ahead of tagging.
4. Create an annotated tag and push it:
   ```sh
   git tag -a v0.1.0 -m "Release v0.1.0"
   git push origin v0.1.0
   ```
5. Verify the GitHub Release attaches artifacts for all three targets (`x86_64-unknown-linux-gnu`, `aarch64-apple-darwin`, `x86_64-apple-darwin`).

## Artifact Naming & Checksum Verification

Each target matrix job produces:
- Archive: `todoist-tui-<version>-<target>.tar.gz`
- Checksum: `todoist-tui-<version>-<target>.tar.gz.sha256`

Verify downloaded archives before installing:
```sh
shasum -a 256 -c todoist-tui-0.1.0-aarch64-apple-darwin.tar.gz.sha256
# On Linux:
sha256sum -c todoist-tui-0.1.0-x86_64-unknown-linux-gnu.tar.gz.sha256
```

## Rollback / Yank Procedure

If a regression or packaging defect is discovered after publishing:
1. Mark the GitHub Release as a pre-release or remove the faulty release assets immediately, and publish a clear warning note in the release body.
2. Delete or avoid reusing the broken tag; cut a forward patch release (`v0.1.1`) documenting the fix under `## [0.1.1]` in `CHANGELOG.md`.
3. If published to crates.io, yank the defective crate version (`cargo yank --vers X.Y.Z`) immediately after publishing the patched version.

## Gate Regression Verification

Every gate in `.github/workflows/ci.yml` fails the build without `continue-on-error` when a deliberate regression is introduced:

| Gate | Command | Deliberate Regression Test & Observed Failure | Date Verified |
|---|---|---|---|
| **fmt** | `cargo fmt --all -- --check` | Unformatted indentation / trailing space causes `Diff in ...` and non-zero exit (`1`). | 2026-11-10 |
| **clippy** | `cargo clippy --all-targets -- -D warnings` | Unused import or `unwrap()` in `src/` triggers `-D warnings` error (`101`). | 2026-11-10 |
| **unit** | `cargo test --all` | Failing unit assertion (e.g. broken label validation or help overlay truncation) exits `101`. | 2026-11-10 |
| **integration** | `cargo test --test offline_online_cycle ...` | Returning wrong exit code / token leak / doc keymap drift fails integration assertion cleanly (`101`). | 2026-11-10 |
| **perf** | `cargo test --release --test perf_gates -- --ignored --nocapture` | Exceeding `MAX_COLD_START` (`500ms`), `MAX_INPUT_TO_RENDER_P95` (`16ms`), or `MAX_SYNC_ROUNDTRIP_P95` (`2s`) panics and exits `101`. | 2026-11-10 |

