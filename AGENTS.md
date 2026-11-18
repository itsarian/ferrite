# Ferrite DM — Agent & Contributor Engineering Guidelines

All agents and contributors working in this repository MUST strictly adhere to [**`docs/CI_CD.md`**](docs/CI_CD.md).

## Mandatory Pre-Flight Verification Rules
Before staging, committing, or proposing code changes:

1. **Formatting**:
   ```bash
   cargo fmt --all -- --check
   ```
   Zero style or formatting drift is permitted.

2. **Linting (Zero Warnings Allowed)**:
   ```bash
   cargo clippy --workspace --all-targets -- -D warnings
   ```
   Every clippy warning is treated as an error. Do not silence warnings with blanket `#![allow(...)]` without strong, documented architectural justification.

3. **Workspace Tests**:
   ```bash
   cargo test --workspace
   ```
   All unit, doc, and integration tests across all workspace crates must pass cleanly.

4. **Documentation**:
   ```bash
   cargo doc --workspace --no-deps
   ```
   Ensure doc comments compile without warnings or broken links.

5. **Packaging & Script Assets** (when modifying `scripts/` or `resources/`):
   ```bash
   swift scripts/generate-icon.swift resources
   ./scripts/package-macos.sh --skip-build
   ```

## Release Protocol (docs/CI_CD.md Section 4)
- Releases must use semantic versioning `vX.Y.Z`.
- Always bump `version` in `Cargo.toml` before tagging.
- Checksums in `SHA256SUMS.txt` must always be generated and verified across all 16 release assets (`ferrite-dm-<version>-<os>-<arch>.<ext>` for macOS `arm64`/`x86_64` and Linux `arm64`/`x86_64`).
- Never push directly to tags without local build validation or dry-run via `workflow_dispatch`.

## Mandatory CI Monitoring & Completion Rule
Whenever you push changes or trigger a CI run:
- **DO NOT end the conversation turn or report completion until all in-flight CI workflow runs have passed successfully (`conclusion: success` / green checkmark).**
- Monitor active runs using `gh run list` or `gh run watch`.
- If any job in CI fails, inspect the failure logs immediately (`gh run view <id> --log-failed`), apply the fix, execute the pre-flight verification checks, commit, push, and continue monitoring until the entire pipeline is 100% green (`✓`).

