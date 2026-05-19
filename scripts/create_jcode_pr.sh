#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'USAGE'
Usage: scripts/create_jcode_pr.sh [--upstream] [gh-pr-create-args...]

Creates a jcode pull request with a safe default target.

Default target:
  ShawnSantiago/jcode

Upstream target:
  Pass --upstream explicitly to target 1jehuang/jcode.

Examples:
  scripts/create_jcode_pr.sh --base master --head my-feature --title "Fix thing" --body "..."
  scripts/create_jcode_pr.sh --upstream --base master --head ShawnSantiago:my-feature --title "Fix thing" --body "..."
USAGE
}

repo="ShawnSantiago/jcode"
args=()

while [[ $# -gt 0 ]]; do
  case "$1" in
    --upstream)
      repo="1jehuang/jcode"
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    --repo|-R)
      echo "error: --repo is managed by this wrapper; use --upstream only when explicitly targeting 1jehuang/jcode" >&2
      exit 2
      ;;
    *)
      args+=("$1")
      shift
      ;;
  esac
done

if [[ ${#args[@]} -eq 0 ]]; then
  usage >&2
  exit 2
fi

echo "Creating PR in ${repo}" >&2
exec gh pr create --repo "${repo}" "${args[@]}"
