#!/usr/bin/env bash
# `omarchy plugin add` clones the repository and refuses any symlink in it;
# pnpm's node_modules is made of them, so validate what a clone would contain.
set -euo pipefail

stage=$(mktemp -d)
trap 'rm -rf -- "$stage"' EXIT

git ls-files -z --cached --others --exclude-standard |
  tar --create --null --files-from=- --ignore-failed-read 2>/dev/null |
  tar --extract --directory="$stage"

"${OMARCHY_PLUGIN_VALIDATE:-omarchy-plugin-validate}" "$stage/"
