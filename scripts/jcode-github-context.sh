#!/usr/bin/env bash
set -euo pipefail

notify() {
  if command -v notify-send >/dev/null 2>&1; then
    notify-send "Jcode GitHub Context" "$1"
  else
    printf 'Jcode GitHub Context: %s\n' "$1" >&2
  fi
}

active_app() {
  if command -v xdotool >/dev/null 2>&1 && [[ -n "${DISPLAY:-}" ]]; then
    xdotool getactivewindow getwindowclassname 2>/dev/null || true
    return
  fi
  if command -v niri >/dev/null 2>&1; then
    niri msg -j focused-window 2>/dev/null | python3 -c 'import json,sys; print((json.load(sys.stdin).get("app_id") or ""))' 2>/dev/null || true
    return
  fi
}

copy_current_url() {
  local before="" url=""
  if command -v wl-paste >/dev/null 2>&1; then
    before="$(wl-paste 2>/dev/null || true)"
  elif command -v xclip >/dev/null 2>&1; then
    before="$(xclip -selection clipboard -o 2>/dev/null || true)"
  fi

  if command -v xdotool >/dev/null 2>&1 && [[ -n "${DISPLAY:-}" ]]; then
    xdotool key --clearmodifiers ctrl+l
    sleep 0.08
    xdotool key --clearmodifiers ctrl+c
  elif command -v wtype >/dev/null 2>&1; then
    wtype -M ctrl l -m ctrl
    sleep 0.08
    wtype -M ctrl c -m ctrl
  else
    notify "Need xdotool on X11 or wtype on Wayland to read the focused browser tab URL."
    exit 1
  fi
  sleep 0.12

  if command -v wl-paste >/dev/null 2>&1; then
    url="$(wl-paste 2>/dev/null || true)"
    printf '%s' "$before" | wl-copy 2>/dev/null || true
  elif command -v xclip >/dev/null 2>&1; then
    url="$(xclip -selection clipboard -o 2>/dev/null || true)"
    printf '%s' "$before" | xclip -selection clipboard 2>/dev/null || true
  fi
  printf '%s\n' "$url"
}

app="$(active_app | tr '[:upper:]' '[:lower:]')"
case "$app" in
  *firefox*|*chrome*|*chromium*|*google-chrome*|*brave*|*vivaldi*|*microsoft-edge*) ;;
  *) notify "Focus a Firefox or Chrome-family browser window first."; exit 1 ;;
esac

url="$(copy_current_url | tr -d '\r')"
if [[ ! "$url" =~ ^https://github\.com/[^/]+/[^/]+/(issues|pull)/[0-9]+ ]]; then
  notify "Current tab is not a GitHub issue or pull request."
  exit 1
fi

workdir="${JCODE_GITHUB_CONTEXT_DEFAULT_CWD:-$PWD}"
if [[ "$url" =~ ^https://github\.com/([^/]+)/([^/]+)/(issues|pull)/([0-9]+) ]]; then
  owner="${BASH_REMATCH[1]}"
  repo="${BASH_REMATCH[2]}"
  kind="${BASH_REMATCH[3]}"
  number="${BASH_REMATCH[4]}"
else
  notify "Could not parse GitHub URL."
  exit 1
fi

if [[ "${owner,,}/${repo,,}" == "1jehuang/jcode" || "${repo,,}" == "jcode" ]]; then
  for candidate in "${JCODE_REPO_DIR:-}" "$HOME/jcode" "$HOME/src/jcode" "$PWD"; do
    if [[ -n "$candidate" && -d "$candidate/.git" ]] && git -C "$candidate" remote -v 2>/dev/null | grep -qi 'github.com[:/]1jehuang/jcode'; then
      workdir="$candidate"
      break
    fi
  done
fi

tmpdir="$(mktemp -d "${TMPDIR:-/tmp}/jcode-github-context.XXXXXX")"
context_file="$tmpdir/context.md"

python3 - "$url" "$owner" "$repo" "$kind" "$number" >"$context_file" <<'PY'
import json, os, sys, urllib.request
url, owner, repo, kind, number = sys.argv[1:]
token = os.environ.get("GITHUB_TOKEN") or os.environ.get("GH_TOKEN")
headers = {"Accept": "application/vnd.github+json", "User-Agent": "jcode-github-context"}
if token:
    headers["Authorization"] = f"Bearer {token}"

def get(path):
    req = urllib.request.Request(f"https://api.github.com/repos/{owner}/{repo}{path}", headers=headers)
    with urllib.request.urlopen(req, timeout=20) as r:
        return json.load(r)

def clip(s, n=12000):
    s = s or ""
    return s if len(s) <= n else s[:n] + "\n\n[truncated]"

try:
    item = get(("/pulls/" if kind == "pull" else "/issues/") + number)
    comments = get(f"/issues/{number}/comments?per_page=20")
except Exception as e:
    print(f"Please work on this GitHub {kind} using the URL below. I could not fetch API details: {e}\n\n{url}")
    sys.exit(0)

label = "pull request" if kind == "pull" else "issue"
print(f"Please work on this GitHub {label}. Start by inspecting the linked context, then make the appropriate code changes and validate them.\n")
print(f"URL: {url}")
print(f"Repository: {owner}/{repo}")
print(f"Title: {item.get('title','')}")
print(f"Author: {item.get('user',{}).get('login','')}")
print(f"State: {item.get('state','')}")
if kind == "pull":
    print(f"Base: {item.get('base',{}).get('ref','')}")
    print(f"Head: {item.get('head',{}).get('ref','')}")
print("\n## Body\n")
print(clip(item.get('body') or '(no body)'))
if comments:
    print("\n## Recent comments\n")
    for c in comments[-10:]:
        print(f"### {c.get('user',{}).get('login','')} at {c.get('created_at','')}\n")
        print(clip(c.get('body'), 6000))
        print()
PY

terminal_cmd=()
if command -v ghostty >/dev/null 2>&1; then
  terminal_cmd=(ghostty -e)
elif command -v x-terminal-emulator >/dev/null 2>&1; then
  terminal_cmd=(x-terminal-emulator -e)
elif command -v gnome-terminal >/dev/null 2>&1; then
  terminal_cmd=(gnome-terminal --)
elif command -v konsole >/dev/null 2>&1; then
  terminal_cmd=(konsole -e)
elif command -v alacritty >/dev/null 2>&1; then
  terminal_cmd=(alacritty -e)
else
  notify "No supported terminal emulator found."
  exit 1
fi

notify "Opening fresh Jcode session for ${owner}/${repo}#${number}."
exec "${terminal_cmd[@]}" jcode -C "$workdir" --startup-message-file "$context_file"
