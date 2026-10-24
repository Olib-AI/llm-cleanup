# Security policy

llm-cleanup parses untrusted documents (`.docx` packages, Markdown, plain text) offline,
with no network access and no runtime model. That makes the parser and the byte-splice
engine the security-critical surface of the project.

## Reporting a vulnerability

Email **security@olib.ai** with a description, a proof-of-concept file if you have one, and
the version/commit you tested. Do not open a public issue for an unpatched vulnerability.
We will acknowledge within 3 working days and aim to ship a fix within 30 days.

## Supported versions

| Version | Supported |
| ------- | --------- |
| 0.2.x   | Yes       |
| 0.1.x   | No        |

## Toolchain and dependency policy

- **Toolchain.** The repository is pinned to Rust **1.99.1** (2026-10-15) in
  `rust-toolchain.toml`; `Cargo.toml` declares `rust-version = "1.99"`. CI and release
  builds use the same pin, so a release is always built by the compiler the repo declares.
  Rust is upgraded deliberately: point releases (1.99.0 → 1.99.1) are adopted as soon as
  they contain soundness or miscompilation fixes, minor releases after a CI soak.
- **`unsafe`.** The entire workspace sets `unsafe_code = "deny"`, and there is no `unsafe`
  block in the tree. A new one fails the build rather than passing review.
- **Auditing.** `cargo audit` runs on every pull request, on every push to `main`, and on a
  daily schedule against the live RustSec advisory database. A newly published
  vulnerability fails the build even if no code changed. Run it locally with
  `scripts/audit.sh`.
- **Triage, not blanket ignores.** `.cargo/audit.toml` may ignore an advisory only when the
  affected code path is unreachable from this workspace, and every ignore records why and
  who owns the fix. The list is reviewed at each release.
- **Freshness.** Dependabot watches the `cargo` ecosystem weekly and GitHub Actions
  monthly. Patch releases of direct dependencies are merged when CI is green; major
  versions are evaluated by hand (parsers especially).

## Current advisory triage

Audited 2026-10-24 against `rustsec/advisory-db` @ 2026-10-24, `Cargo.lock` committed.

Fixed in this release:

| Advisory | Crate | Action |
| -------- | ----- | ------ |
| RUSTSEC-2026-0194 | `quick-xml` (direct) | Quadratic attribute-parsing DoS on untrusted XML — **upgraded 0.38.4 → 0.41.0**. This is the parser that reads `word/document.xml` from an untrusted `.docx`. |
| RUSTSEC-2026-0195 | `quick-xml` (direct) | Unbounded namespace allocation — **upgraded 0.38.4 → 0.41.0**. |
| RUSTSEC-2026-0204 | `crossbeam-epoch` | Invalid pointer dereference in `fmt::Pointer` — **upgraded 0.9.18 → 0.9.21**. |
| RUSTSEC-2026-0190 | `anyhow` (direct) | Unsoundness in `Error::downcast_mut()` — **upgraded 1.0.102 → 1.0.104** and the requirement in `Cargo.toml` now states the patched minimum. |
| RUSTSEC-2026-0221 | `event-listener` | `!Send` tags can cross thread boundaries — **upgraded 5.4.1 → 5.4.2**. |
| RUSTSEC-2026-0186 | `memmap2` | Unchecked pointer offset in `advise_range`/`flush_range` — **upgraded 0.9.10 → 0.9.12**. |
| RUSTSEC-2026-0257 | `webbrowser` | Unix `BROWSER` argument injection in `webbrowser::open*` — **upgraded 1.2.1 → 1.2.5**. Slint's winit backend is the only user; the workspace never opens a URL itself. |
| yanked | `clipboard-win` | The lock pinned a yanked 5.4.1; **moved to 5.4.3**. |

Reviewed and accepted (not reachable):

| Advisory | Crate | Why it is not reachable | Revisit |
| -------- | ----- | ----------------------- | ------- |
| RUSTSEC-2026-0367 / -0368 / -0369 | `zip` 2.4.2, 0.6.6 | All three are `extract()`-only (SUID/SGID preservation, symlink target escapes). We never extract archives to disk: entries are read into memory and written back with `raw_copy_file`. | Blocked on `zip` 9: it changes `ZipFile::name()` to return a `Result<PathBuf>` and adds `typed-path`, so the DOCX adapter needs a real migration, not a version bump (tracked for 0.3). |
| RUSTSEC-2026-0194 / -0195 | `quick-xml` 0.36.2, 0.39.4 | Older copies used by docx-rs (export/write path) and wayland-scanner/zbus_xml (Linux integration, trusted input). Our reader is on 0.41.0. | When docx-rs ≥ 0.4.21 lands |

Informational advisories (unmaintained/unsound transitive crates) are printed by
`cargo audit` but do not fail the build; they are tracked by Dependabot.

## Threat model notes

- **Malicious documents.** Parsing must be linear-time and memory-bounded: no re-serialising
  of untrusted XML, no entity expansion (we use a pull reader, never a DOM), and every edit
  is bounds-checked before it is spliced (`cleanup_core::splice`).
- **Corrupt output is a security bug.** If the post-edit structural self-check fails, the
  tool writes nothing and exits non-zero. A partially cleaned or reflowed document is never
  emitted.
- **No network, no telemetry.** The binaries make no outbound connections. The GUI binary is
  `#![windows_subsystem = "windows"]` on Windows and links no networking stack of its own.
- **Windows runtime.** The Windows GUI is built against the dynamic CRT and ships the
  Microsoft Visual C++ runtime app-local (see `scripts/package-windows.ps1`), so the app runs
  on a clean Windows install without a redistributable prompt, and no DLL is loaded from an
  ambient search path first.
