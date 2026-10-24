#!/usr/bin/env python3
"""Publish captured CI/workspace logs and optional git patches to GitHub Check Runs.

Why this exists:
GitHub Actions' raw log (`gh run view --log`) and artifact (`gh run download`) endpoints
redirect from `api.github.com` to `*.actions.githubusercontent.com` blob storage, which
is blocked in restricted/sandboxed agent environments that only allow `github.com` and
`api.github.com`. Check Run objects (`output.title`, `output.summary`, `output.text`),
by contrast, are served directly as JSON from `https://api.github.com/repos/.../check-runs`.

This script:
  1. Strips ANSI terminal escapes so logs render cleanly inside Markdown code fences,
  2. Writes the summary + output to `$GITHUB_STEP_SUMMARY` for human UI viewers,
  3. Encodes optional `git diff` patches (`--patch-file`) losslessly via `base64-gzip`
     so an agent without local `rustfmt` or `cargo` can extract and `git apply` them,
  4. Publishes one or more Check Runs via `POST /repos/{owner}/{repo}/check-runs`,
     automatically splitting `output.text` below GitHub's 65,535-byte limit.
"""

from __future__ import annotations

import argparse
import base64
import gzip
import json
import os
import re
import sys
import urllib.error
import urllib.request
from pathlib import Path

ANSI_ESCAPE_RE = re.compile(r"\x1B(?:[@-Z\\-_]|\[[0-?]*[ -/]*[@-~])")
# Keep well under GitHub's 65,535-character / byte ceiling for `output.summary` & `output.text`.
MAX_FIELD_BYTES = 58_000
MAX_CHUNKS = 6


def strip_ansi(text: str) -> str:
    cleaned = ANSI_ESCAPE_RE.sub("", text)
    return cleaned.replace("\r\n", "\n")


def utf8_truncate(text: str, max_bytes: int) -> str:
    encoded = text.encode("utf-8", errors="replace")
    if len(encoded) <= max_bytes:
        return text
    truncated = encoded[: max_bytes - 64].decode("utf-8", errors="ignore")
    return truncated + "\n... [truncated]"


def chunk_text_utf8(text: str, max_bytes: int) -> list[str]:
    if not text:
        return [""]
    lines = text.splitlines(keepends=True)
    chunks: list[str] = []
    cur: list[str] = []
    cur_bytes = 0
    for line in lines:
        lb = len(line.encode("utf-8", errors="replace"))
        if lb > max_bytes:
            if cur:
                chunks.append("".join(cur))
                cur, cur_bytes = [], 0
            # Hard-split an abnormally long single line safely on UTF-8 boundaries.
            raw = line.encode("utf-8", errors="replace")
            for i in range(0, len(raw), max_bytes):
                chunks.append(raw[i : i + max_bytes].decode("utf-8", errors="ignore"))
            continue
        if cur_bytes + lb > max_bytes and cur:
            chunks.append("".join(cur))
            cur, cur_bytes = [], 0
        cur.append(line)
        cur_bytes += lb
    if cur:
        chunks.append("".join(cur))
    return chunks or [""]


def format_patch_section(patch_bytes: bytes) -> str:
    if not patch_bytes:
        return ""
    compressed = gzip.compress(patch_bytes, compresslevel=9)
    b64 = base64.b64encode(compressed).decode("ascii")
    wrapped_b64 = "\n".join(b64[i : i + 100] for i in range(0, len(b64), 100))

    parts = [
        "### Generated Patch (`git apply` ready)",
        f"Patch size: `{len(patch_bytes)}` bytes raw (`{len(compressed)}` bytes gzip).",
        "Extract with `scripts/gh-workspace.sh extract-patch` or decode the `base64-gzip` block:",
        f"```base64-gzip\n{wrapped_b64}\n```",
    ]
    if len(patch_bytes) <= 24_000:
        preview = patch_bytes.decode("utf-8", errors="replace")
        parts.append(f"```diff\n{preview.rstrip()}\n```")
    return "\n\n".join(parts)


def post_check_run(
    repo: str,
    token: str,
    name: str,
    head_sha: str,
    conclusion: str,
    title: str,
    summary: str,
    text: str,
) -> int | None:
    url = f"https://api.github.com/repos/{repo}/check-runs"
    payload = {
        "name": name[:100],
        "head_sha": head_sha,
        "status": "completed",
        "conclusion": conclusion,
        "output": {
            "title": title[:250],
            "summary": utf8_truncate(summary, MAX_FIELD_BYTES),
            "text": utf8_truncate(text, MAX_FIELD_BYTES),
        },
    }
    data = json.dumps(payload).encode("utf-8")
    req = urllib.request.Request(
        url,
        data=data,
        method="POST",
        headers={
            "Accept": "application/vnd.github+json",
            "Authorization": f"Bearer {token}",
            "Content-Type": "application/json",
            "X-GitHub-Api-Version": "2022-11-28",
            "User-Agent": "llm-cleanup-workspace-reporter",
        },
    )
    try:
        with urllib.request.urlopen(req, timeout=30) as resp:
            body = json.loads(resp.read().decode("utf-8", errors="replace"))
            return body.get("id")
    except urllib.error.HTTPError as exc:
        err_body = exc.read().decode("utf-8", errors="replace")
        if exc.code in (401, 403):
            print(
                f"notice: Check Run API returned HTTP {exc.code} (likely read-only fork token); "
                f"report remains in GITHUB_STEP_SUMMARY. Details: {err_body}",
                file=sys.stderr,
            )
            return None
        print(
            f"warning: failed to publish Check Run '{name}' (HTTP {exc.code}): {err_body}",
            file=sys.stderr,
        )
        return None
    except Exception as exc:  # pragma: no cover - network resilience
        print(f"warning: failed to publish Check Run '{name}': {exc}", file=sys.stderr)
        return None


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--name", required=True, help="Check Run name")
    parser.add_argument("--title", required=True, help="Short output title")
    parser.add_argument(
        "--conclusion",
        default="neutral",
        choices=["success", "failure", "neutral", "cancelled", "skipped", "timed_out"],
    )
    parser.add_argument("--head-sha", default="", help="Commit SHA to attach check run to")
    parser.add_argument("--summary", default="", help="Inline Markdown summary")
    parser.add_argument("--summary-file", default="", help="Path to Markdown summary file")
    parser.add_argument("--log-file", default="", help="Path to captured command log")
    parser.add_argument("--patch-file", default="", help="Path to optional git diff patch")
    args = parser.parse_args()

    summary_parts: list[str] = []
    if args.summary:
        summary_parts.append(args.summary.strip())
    if args.summary_file and Path(args.summary_file).is_file():
        summary_parts.append(
            strip_ansi(Path(args.summary_file).read_text(encoding="utf-8", errors="replace")).strip()
        )
    summary_md = "\n\n".join(p for p in summary_parts if p) or f"**Result:** `{args.conclusion}`"

    log_text = ""
    if args.log_file and Path(args.log_file).is_file():
        log_text = strip_ansi(Path(args.log_file).read_text(encoding="utf-8", errors="replace")).strip()

    patch_section = ""
    if args.patch_file and Path(args.patch_file).is_file():
        patch_bytes = Path(args.patch_file).read_bytes()
        if patch_bytes.strip():
            patch_section = format_patch_section(patch_bytes)

    # Write to GitHub Step Summary for browser UI visibility.
    step_summary_path = os.environ.get("GITHUB_STEP_SUMMARY")
    if step_summary_path:
        try:
            with open(step_summary_path, "a", encoding="utf-8") as fh:
                fh.write(f"## {args.name} — {args.title}\n\n")
                fh.write(summary_md + "\n\n")
                if patch_section:
                    fh.write(patch_section + "\n\n")
                if log_text:
                    fh.write("<details><summary>Captured Command Log</summary>\n\n")
                    fh.write(f"```text\n{utf8_truncate(log_text, 120_000)}\n```\n\n</details>\n\n")
        except OSError as exc:
            print(f"warning: could not write GITHUB_STEP_SUMMARY: {exc}", file=sys.stderr)

    repo = os.environ.get("GITHUB_REPOSITORY", "")
    token = os.environ.get("GITHUB_TOKEN", "")
    head_sha = args.head_sha.strip() or os.environ.get("GITHUB_SHA", "")
    if not (repo and token and head_sha):
        print("Skipping GitHub Check Run API publish (missing GITHUB_REPOSITORY/GITHUB_TOKEN/SHA).")
        return 0

    body_sections: list[str] = []
    if patch_section:
        body_sections.append(patch_section)
    if log_text:
        log_chunks = chunk_text_utf8(log_text, MAX_FIELD_BYTES - 8_000)
        if len(log_chunks) > MAX_CHUNKS:
            # Keep first chunk (context/setup) and last MAX_CHUNKS-1 chunks (errors/results).
            kept = [log_chunks[0]] + log_chunks[-(MAX_CHUNKS - 1) :]
            log_chunks = kept
    else:
        log_chunks = [""]

    total = len(log_chunks)
    for idx, chunk in enumerate(log_chunks, start=1):
        run_name = args.name if total == 1 else f"{args.name} ({idx}/{total})"
        text_parts: list[str] = []
        if idx == 1 and patch_section:
            text_parts.append(patch_section)
        if chunk:
            fence = "~~~~text"
            text_parts.append(f"### Command Output ({idx}/{total})\n\n{fence}\n{chunk.rstrip()}\n~~~~")
        full_text = "\n\n".join(text_parts) if text_parts else "_No command output captured._"
        check_id = post_check_run(
            repo=repo,
            token=token,
            name=run_name,
            head_sha=head_sha,
            conclusion=args.conclusion,
            title=args.title if total == 1 else f"{args.title} [part {idx}/{total}]",
            summary=summary_md,
            text=full_text,
        )
        if check_id:
            print(f"Published Check Run '{run_name}' (id={check_id}) on {head_sha[:12]}")

    return 0


if __name__ == "__main__":
    raise SystemExit(main())
