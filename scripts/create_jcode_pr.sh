#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'USAGE'
Usage: scripts/create_jcode_pr.sh [--upstream] [--allow-master-head] [gh-pr-create-args...]

Creates a jcode pull request with safe defaults.

Default target:
  ShawnSantiago/jcode

Upstream target:
  Pass --upstream explicitly to target 1jehuang/jcode.

Branch safety:
  PRs from master are blocked by default. Use a feature branch, or pass
  --allow-master-head only when explicitly approved.

Examples:
  scripts/create_jcode_pr.sh --base master --head my-feature --title "Fix thing" --body "..."
  scripts/create_jcode_pr.sh --upstream --base master --head ShawnSantiago:my-feature --title "Fix thing" --body "..."
USAGE
}

repo="ShawnSantiago/jcode"
allow_master_head=false
args=()
head_arg=""

while [[ $# -gt 0 ]]; do
  case "$1" in
    --upstream)
      repo="1jehuang/jcode"
      shift
      ;;
    --allow-master-head)
      allow_master_head=true
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    --repo|-R|--repo=*|-R=*)
      echo "error: --repo is managed by this wrapper; use --upstream only when explicitly targeting 1jehuang/jcode" >&2
      exit 2
      ;;
    --head)
      if [[ $# -lt 2 ]]; then
        echo "error: --head requires a value" >&2
        exit 2
      fi
      head_arg="$2"
      args+=("$1" "$2")
      shift 2
      ;;
    --head=*)
      head_arg="${1#--head=}"
      args+=("$1")
      shift
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

if [[ -z "${head_arg}" ]]; then
  head_arg="$(git branch --show-current 2>/dev/null || true)"
fi
head_branch="${head_arg##*:}"

if [[ "${allow_master_head}" != true && "${head_branch}" == "master" ]]; then
  cat >&2 <<'ERROR'
error: refusing to create a PR from master.
Create a feature branch first, or pass --allow-master-head only when explicitly approved.
ERROR
  exit 2
fi

echo "Creating PR in ${repo} from head ${head_arg:-${head_branch}}" >&2
exec gh pr create --repo "${repo}" "${args[@]}"
