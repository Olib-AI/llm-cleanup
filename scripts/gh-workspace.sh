#!/usr/bin/env bash
# =============================================================================
# gh-workspace.sh — Local driver for the GitHub Actions Rust CI/CD Workspace
# =============================================================================
# Bridges environments without local `rustc`/`cargo`/`crates.io` (or where
# `*.actions.githubusercontent.com` log/artifact redirects are firewalled) to
# `.github/workflows/workspace.yml` via `api.github.com` Check Runs.
#
# Usage:
#   scripts/gh-workspace.sh doctor
#   scripts/gh-workspace.sh status [ref_or_pr_number]
#   scripts/gh-workspace.sh wait [ref_or_pr_number] [--timeout 900]
#   scripts/gh-workspace.sh dispatch <mode> [--branch BR] [--target-ref REF] [--package PKG] [--args ARGS] [--os OS]
#   scripts/gh-workspace.sh extract-patch [ref_or_check_id] [--out patchfile] [--apply]

set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

REPO="${GH_REPO:-$(gh repo view --json nameWithOwner --jq .nameWithOwner 2>/dev/null || echo "Olib-AI/llm-cleanup")}"

resolve_sha() {
  local target="${1:-HEAD}"
  if [[ "$target" =~ ^[0-9]+$ ]]; then
    gh api "repos/$REPO/pulls/$target" --jq '.head.sha'
  elif [[ "$target" == "HEAD" ]]; then
    git rev-parse HEAD
  else
    gh api "repos/$REPO/commits/$target" --jq '.sha' 2>/dev/null || git rev-parse "$target"
  fi
}

cmd_doctor() {
  echo "== Local Rust & Sandbox Environment Check =="
  for tool in rustc cargo rustup rustfmt cargo-clippy cargo-audit; do
    if command -v "$tool" >/dev/null 2>&1; then
      echo "  [present] $tool -> $(command -v "$tool")"
    else
      echo "  [missing] $tool"
    fi
  done

  echo ""
  echo "== Outbound Host Reachability =="
  for url in \
    "https://api.github.com" \
    "https://github.com" \
    "https://index.crates.io/config.json" \
    "https://static.rust-lang.org" \
    "https://results-receiver.actions.githubusercontent.com"; do
    if curl -sSI --max-time 4 "$url" >/dev/null 2>&1; then
      echo "  [reachable] $url"
    else
      echo "  [blocked]   $url"
    fi
  done

  echo ""
  echo "== GitHub API Repository Access ($REPO) =="
  gh api "repos/$REPO" --jq '"  repo=\(.full_name) default_branch=\(.default_branch) push=\(.permissions.push) admin=\(.permissions.admin)"'
}

cmd_status() {
  local ref="${1:-HEAD}"
  local sha
  sha="$(resolve_sha "$ref")"
  echo "== Check Runs for $ref ($sha) =="
  local payload
  payload="$(gh api "repos/$REPO/commits/$sha/check-runs?per_page=100")"
  CHECKS_JSON="$payload" python3 - <<'PY'
import json, os, sys

data = json.loads(os.environ["CHECKS_JSON"])
runs = data.get("check_runs", [])
if not runs:
    print("  (no check runs found yet)")
    sys.exit(0)

for r in runs:
    status = r.get("status")
    concl = r.get("conclusion") or "-"
    out = r.get("output") or {}
    title = out.get("title") or ""
    name = r.get("name")
    rid = r.get("id")
    print(f"- [{status}/{concl}] {name} (id={rid}) {title}")

print("")
for r in runs:
    out = r.get("output") or {}
    summary = (out.get("summary") or "").strip()
    text = (out.get("text") or "").strip()
    if summary or text:
        name = r.get("name")
        rid = r.get("id")
        print("=" * 80)
        print(f"REPORT: {name} (id={rid}, conclusion={r.get('conclusion')})")
        print("=" * 80)
        if summary:
            print(summary)
            print("")
        if text:
            print(text)
            print("")
PY
}

cmd_wait() {
  local ref="HEAD"
  local timeout_secs=900
  while [[ $# -gt 0 ]]; do
    case "$1" in
      --timeout)
        timeout_secs="$2"
        shift 2
        ;;
      *)
        ref="$1"
        shift
        ;;
    esac
  done

  local sha
  sha="$(resolve_sha "$ref")"
  echo "Waiting up to ${timeout_secs}s for GitHub Actions on $ref ($sha)..."

  local elapsed=0
  local seen_any=0
  while (( elapsed < timeout_secs )); do
    local raw_checks state
    raw_checks="$(gh api "repos/$REPO/commits/$sha/check-runs?per_page=100")"
    state="$(CHECKS_JSON="$raw_checks" python3 - <<'PY'
import json, os
runs = json.loads(os.environ["CHECKS_JSON"]).get("check_runs", [])
total = len(runs)
active = [r["name"] for r in runs if r.get("status") != "completed"]
failed = [
    r["name"] for r in runs
    if r.get("status") == "completed" and r.get("conclusion") not in ("success", "skipped", "neutral")
]
print(json.dumps({"total": total, "active": active, "failed": failed}))
PY
)"
    local total active_count failed_count
    total="$(echo "$state" | python3 -c 'import json,sys; print(json.load(sys.stdin)["total"])')"
    active_count="$(echo "$state" | python3 -c 'import json,sys; print(len(json.load(sys.stdin)["active"]))')"
    failed_count="$(echo "$state" | python3 -c 'import json,sys; print(len(json.load(sys.stdin)["failed"]))')"

    if (( total > 0 )); then
      seen_any=1
    fi
    if (( seen_any == 1 && active_count == 0 )); then
      echo "All $total check run(s) completed on ${sha:0:12}."
      cmd_status "$sha"
      (( failed_count == 0 )) && return 0 || return 1
    fi

    echo "  [${elapsed}s] total=$total in_progress=$active_count"
    sleep 12
    elapsed=$(( elapsed + 12 ))
  done

  echo "Timed out after ${timeout_secs}s waiting for checks on $sha." >&2
  cmd_status "$sha"
  return 2
}

cmd_dispatch() {
  local mode="${1:-full}"
  shift || true
  local branch
  branch="$(git rev-parse --abbrev-ref HEAD)"
  local target_ref="" pkg="" args="" os_choice="ubuntu-latest"

  while [[ $# -gt 0 ]]; do
    case "$1" in
      --branch) branch="$2"; shift 2 ;;
      --target-ref) target_ref="$2"; shift 2 ;;
      --package|-p) pkg="$2"; shift 2 ;;
      --args) args="$2"; shift 2 ;;
      --os) os_choice="$2"; shift 2 ;;
      *) echo "Unknown flag: $1" >&2; exit 2 ;;
    esac
  done

  if gh workflow view workspace.yml --repo "$REPO" >/dev/null 2>&1; then
    echo "Dispatching workspace.yml on branch '$branch' (mode=$mode, os=$os_choice)..."
    gh workflow run workspace.yml \
      --repo "$REPO" \
      --ref "$branch" \
      -f "mode=$mode" \
      -f "target_ref=$target_ref" \
      -f "package=$pkg" \
      -f "args=$args" \
      -f "os=$os_choice"
  else
    local directive="[ws:mode=$mode"
    [[ -n "$target_ref" ]] && directive+=",ref=$target_ref"
    [[ -n "$pkg" ]] && directive+=",pkg=$pkg"
    [[ -n "$os_choice" && "$os_choice" != "ubuntu-latest" ]] && directive+=",os=$os_choice"
    [[ -n "$args" ]] && directive+=",args=$args"
    directive+="]"
    echo "note: workspace.yml is not yet on the default branch (main), so workflow_dispatch is unavailable."
    echo "Push a commit to '$branch' with directive: $directive"
  fi
}

cmd_extract_patch() {
  local target="HEAD"
  local out_file=""
  local apply_patch=0

  while [[ $# -gt 0 ]]; do
    case "$1" in
      --out) out_file="$2"; shift 2 ;;
      --apply) apply_patch=1; shift ;;
      *) target="$1"; shift ;;
    esac
  done

  local json_payload
  if [[ "$target" =~ ^[0-9]{8,}$ ]]; then
    json_payload="$(gh api "repos/$REPO/check-runs/$target")"
  else
    local sha
    sha="$(resolve_sha "$target")"
    json_payload="$(gh api "repos/$REPO/commits/$sha/check-runs?per_page=100")"
  fi

  local tmp_patch
  tmp_patch="$(mktemp)"
  trap 'rm -f "$tmp_patch"' RETURN

  CHECKS_JSON="$json_payload" python3 - "$tmp_patch" <<'PY'
import base64, gzip, json, os, re, sys
from pathlib import Path

out_path = Path(sys.argv[1])
data = json.loads(os.environ["CHECKS_JSON"])
runs = data["check_runs"] if "check_runs" in data else [data]

for r in runs:
    text = ((r.get("output") or {}).get("text") or "")
    m = re.search(r"```base64-gzip\s+([A-Za-z0-9+/=\s]+?)```", text)
    if m:
        raw_b64 = re.sub(r"\s+", "", m.group(1))
        patch_bytes = gzip.decompress(base64.b64decode(raw_b64))
        out_path.write_bytes(patch_bytes)
        print(f"Extracted {len(patch_bytes)} bytes from Check Run '{r.get('name')}' (id={r.get('id')}).", file=sys.stderr)
        sys.exit(0)

print("error: no base64-gzip patch block found in check runs.", file=sys.stderr)
sys.exit(1)
PY

  if [[ -n "$out_file" ]]; then
    cp "$tmp_patch" "$out_file"
    echo "Saved patch to $out_file"
  fi

  if (( apply_patch == 1 )); then
    git apply --stat "$tmp_patch"
    git apply "$tmp_patch"
    echo "Applied patch cleanly to working tree."
  elif [[ -z "$out_file" ]]; then
    cat "$tmp_patch"
  fi
}

subcmd="${1:-status}"
shift || true
case "$subcmd" in
  doctor) cmd_doctor "$@" ;;
  status) cmd_status "$@" ;;
  wait) cmd_wait "$@" ;;
  dispatch) cmd_dispatch "$@" ;;
  extract-patch) cmd_extract_patch "$@" ;;
  *)
    echo "Usage: $0 {doctor|status|wait|dispatch|extract-patch} [args...]" >&2
    exit 2
    ;;
esac
