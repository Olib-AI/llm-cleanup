# GitHub Actions as the Rust CI/CD Workspace

Contributors and coding agents working inside restricted sandboxes may not have local access to the pinned Rust toolchain or outbound package registries. This document records those concrete limitations and describes the workflows and scripts that turn GitHub Actions into a complete remote Rust workspace for testing, building, auditing, running `aiclean`, formatting, and updating `Cargo.lock`.

---

## 1. Sandbox & Local Rust Limitations

Running `scripts/gh-workspace.sh doctor` inside the sandbox identifies five concrete constraints:

1. **No local Rust toolchain or cargo subcommands.**
   `rustc`, `cargo`, `rustup`, `rustfmt`, `cargo-clippy`, and `cargo-audit` are not pre-installed in the container (`~/.cargo` and `/usr/share/cargo` are absent). The workspace requires **Rust `1.99.1`** (`edition = "2024"`, `resolver = "3"`, `rust-version = "1.99"` in `Cargo.toml`, pinned in `rust-toolchain.toml`).
2. **Rustup and crates.io are blocked by the outbound firewall.**
   Outbound network access is allowlisted to `github.com`, `codeload.github.com`, `api.github.com`, `registry.npmjs.org`, `pypi.org`, and `files.pythonhosted.org`. Connections to `sh.rustup.rs`, `static.rust-lang.org`, `index.crates.io`, and `static.crates.io` are terminated at TLS handshake, and dependencies are not vendored in `.cargo/vendor`, so crates and lockfiles cannot be resolved locally.
3. **OS package mirrors (`deb.debian.org`) are blocked.**
   `cleanup-gui` (Slint desktop UI) links against Linux OpenGL/EGL/Wayland/X11/Xkb/Fontconfig development headers (`libgl1-mesa-dev`, `libegl1-mesa-dev`, `libxkbcommon-dev`, `libwayland-dev`, `libxcb1-dev`, `libx11-dev`, `libxrandr-dev`, `libxi-dev`, `libxcursor-dev`, `libfontconfig1-dev`), which cannot be installed via `apt-get` in the sandbox.
4. **Single host OS locally.**
   macOS `.app` bundle creation (`scripts/bundle-macos.sh`), Windows static-CRT CLI builds (`+crt-static`), and Windows app-local Visual C++ runtime staging (`scripts/package-windows.ps1 -Verify`) require `macos-latest` and `windows-latest` runners.
5. **GitHub Actions raw log & artifact blob storage redirects are blocked.**
   `gh run view --log` and `gh run download` issue HTTP 302 redirects from `api.github.com` to `results-receiver.actions.githubusercontent.com` / `pipelines.actions.githubusercontent.com`, which are outside the outbound allowlist (`EOF`). By contrast, **GitHub Check Run objects** (`GET /repos/{owner}/{repo}/check-runs/{id}`) are served directly as JSON from `api.github.com`.

---

## 2. CI/CD Workspace Architecture

### `.github/workflows/workspace.yml` (`Rust Workspace`)
A multi-mode workflow triggered via `workflow_dispatch` (once on `main`) or automatically on `push` to working branches (`arena/**`, `feat/**`, `fix/**`, `ci/**`, `chore/**`, `dev/**`):

| Mode | What it runs |
| --- | --- |
| `full` *(default)* | Toolchain pin check (`1.99.1`), `cargo fmt --check`, `cargo clippy -- -D warnings`, `cargo test --workspace`, `cargo check --no-default-features`, `cargo audit --deny unsound`, release build (`aiclean` + `aiclean-gui`), and `aiclean` CLI smoke tests on `.md`, `.txt`, and `.docx` samples |
| `check` | Fast `rustfmt` check + `clippy` (`-D warnings`) + `--no-default-features` check (supports optional `-p <package>`) |
| `test` | `cargo test --locked` across the workspace or `-p <package>` with optional test filter `args` |
| `build` | Cross-OS release build (`ubuntu-latest`, `macos-latest`, `windows-latest`, or `all`) plus macOS `.app` bundle and Windows MSVC runtime verification |
| `audit` | `scripts/audit.sh` + `cargo audit --deny unsound` against the live RustSec database |
| `run` | Builds `aiclean` (`--release`) and runs arbitrary CLI subcommands (`args`, e.g. `clean samples/docx/report.docx --diff`) or the built-in CLI smoke suite |
| `fmt-diff` | Runs `cargo fmt --all` and publishes a lossless `base64-gzip` `git diff` patch to Check Run output |
| `lockfile-diff` | Runs `cargo update` (optional `args`) + unlocked `cargo check` / `clippy` / `test` / `audit`, and publishes the updated `Cargo.lock` patch as `base64-gzip` in Check Run output |

#### Inspecting Arbitrary Refs / PRs (`target_ref`)
Passing `target_ref` (via `workflow_dispatch` or commit directive `[ws:mode=check,ref=refs/pull/6/head]`) fetches and detaches that ref inside the runner while still publishing the Check Run report onto the current branch's commit SHA, allowing read-only diagnosis of third-party or Dependabot PRs without modifying their branches.

#### Branch-Push Directives (`[ws:...]`)
Before `workspace.yml` is on the default branch (or whenever pushing commits directly to a working branch), include a bracketed directive anywhere in the commit message:

- `[ws:mode=check]` — fast lint & check only
- `[ws:mode=check,ref=refs/pull/6/head]` — diagnose PR #6's exact compiler/clippy output
- `[ws:mode=run,args=coverage --provider anthropic]` — execute `aiclean` CLI and return stdout
- `[ws:mode=lockfile-diff,args=-p zip]` — regenerate `Cargo.lock` and return a `git apply`-ready patch
- `[ws:mode=build,os=all]` — verify Linux, macOS, and Windows release builds & bundles
- `[ws:skip]` — skip the workspace run on a documentation-only push

### `.github/scripts/publish_check_report.py`
Invoked with `if: always()` across `workspace.yml`, `ci.yml`, and `security.yml` using `permissions: checks: write`. It strips ANSI color escapes, writes browser-friendly summaries to `$GITHUB_STEP_SUMMARY`, encodes optional `.patch` files as `base64-gzip`, and posts UTF-8-chunked `output.summary` + `output.text` payloads to `POST /repos/{owner}/{repo}/check-runs` so full logs are always accessible over `api.github.com`.

### Enhancements to Existing Workflows
- **`.github/workflows/ci.yml`**: Added `workflow_dispatch`, upgraded actions (`actions/checkout@v7`, `actions/upload-artifact@v7`), and added Check Run log publishing for `lint-and-test` and `build`.
- **`.github/workflows/security.yml`**: Upgraded actions (`actions/checkout@v7`, `actions/upload-artifact@v7`) and added Check Run log publishing for `cargo-audit`.
- **`.github/workflows/release.yml`**: Added `workflow_dispatch` so cross-architecture CLI (`x86_64`/`aarch64` Linux `cross`, `x86_64`/`aarch64` macOS, Windows static-CRT) and desktop GUI release builds can be dry-run prior to tagging (`GitHub Release` creation remains gated on `refs/tags/v*`).

---

## 3. Local CLI Usage (`scripts/gh-workspace.sh`)

```bash
# 1. Verify sandbox limitations and GitHub API connectivity
./scripts/gh-workspace.sh doctor

# 2. Wait for active checks on HEAD (or a PR number) and print full logs via api.github.com
./scripts/gh-workspace.sh wait HEAD
./scripts/gh-workspace.sh status 6

# 3. Trigger a workflow_dispatch run (once workspace.yml is on main)
./scripts/gh-workspace.sh dispatch test --package cleanup-formats
./scripts/gh-workspace.sh dispatch run --args "diff samples/markdown/article.md"
./scripts/gh-workspace.sh dispatch build --os all

# 4. Extract and apply a generated rustfmt or Cargo.lock patch from a Check Run
./scripts/gh-workspace.sh extract-patch HEAD --apply
```
