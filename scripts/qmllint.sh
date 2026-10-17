#!/usr/bin/env bash
# Lints the plugin QML against the installed Omarchy shell and fails on any
# warning outside the categories the shell's own plugins produce. The plugin
# imports the shell as `qs`, so qmllint gets an import root with that name in
# a temp dir: plugin validation rejects any symlink in the tree.
set -euo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
shell_dir=${OMARCHY_PATH:-/usr/share/omarchy}/shell

imports=$(mktemp -d)
trap 'rm -rf -- "$imports"' EXIT
ln -s "$shell_dir" "$imports/qs"

lint() {
  # Nested Style tokens and facade members are untyped to qmllint.
  /usr/lib/qt6/bin/qmllint -I "$imports" --max-warnings 0 \
    --missing-property disable --unqualified disable "$@"
}

shopt -s extglob
lint "$root"/shell/!(ServiceWindows).qml
# Offscreen tooling has no PanelWindow backend; only this file creates one.
lint --uncreatable-type disable "$root/shell/ServiceWindows.qml"
