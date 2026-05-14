#!/usr/bin/env bash
set -euo pipefail

repo_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
bin_dir="${HOME}/.local/bin"
apps_dir="${XDG_DATA_HOME:-${HOME}/.local/share}/applications"
mkdir -p "$bin_dir" "$apps_dir"
install -m 0755 "$repo_dir/scripts/jcode-github-context.sh" "$bin_dir/jcode-github-context"
install -m 0644 "$repo_dir/packaging/linux/jcode-github-context.desktop" "$apps_dir/jcode-github-context.desktop"

if command -v update-desktop-database >/dev/null 2>&1; then
  update-desktop-database "$apps_dir" >/dev/null 2>&1 || true
fi

cat <<EOF
Installed Jcode GitHub Context launcher.

Binary:  $bin_dir/jcode-github-context
Desktop: $apps_dir/jcode-github-context.desktop

Bind it to a global shortcut or launch "Jcode GitHub Context" while focused on a
Firefox/Chrome GitHub issue or pull request tab.
EOF
