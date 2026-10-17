#!/usr/bin/env bash
set -euo pipefail

here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
root=$(cd "$here/../.." && pwd)
out=$(mkdir -p "${1:-$root/target/render}" && cd "${1:-$root/target/render}" && pwd)
export OMARCHY_PATH=${OMARCHY_PATH:-/usr/share/omarchy}
shell_dir=$OMARCHY_PATH/shell

source=$here/mock-ks-fit
mock=1
if [[ -n ${KS_FIT_RENDER_HELPER:-} ]]; then
  source=$(realpath "$KS_FIT_RENDER_HELPER")
  mock=0
fi

tmp=$(mktemp -d "${XDG_RUNTIME_DIR:-/tmp}/ks-fit-render.XXXXXX")
trap 'rm -rf -- "$tmp"' EXIT

mkdir -p "$tmp/config" "$tmp/bin" "$tmp/helper" "$tmp/data" "$tmp/state" "$tmp/xdg-config" "$tmp/cache"
ln -s "$shell_dir/Commons" "$tmp/config/Commons"
ln -s "$shell_dir/Ui" "$tmp/config/Ui"
cp "$here/harness.qml" "$tmp/config/shell.qml"

for stub in omarchy-notification-send omarchy-bluetooth-power omarchy-restart-shell \
  omarchy-launch-floating-terminal-with-presentation omarchy-shell; do
  printf '#!/usr/bin/env bash\nprintf "%%s\\n" "$(basename "$0") $*" >>"%s"\n' "$tmp/calls.log" >"$tmp/bin/$stub"
  chmod +x "$tmp/bin/$stub"
done
: >"$tmp/calls.log"
if ((mock)); then
  : >"$tmp/held-lock"
fi

PATH="$tmp/bin:$PATH" \
  PLUGIN_DIR="$root" \
  OUT_DIR="$out" \
  KS_FIT_HELPER="$tmp/helper/ks-fit" \
  KS_FIT_RENDER_SOURCE="$source" \
  KS_FIT_RENDER_MOCK="$mock" \
  KS_FIT_MOCK_HELD_LOCK="$tmp/held-lock" \
  XDG_DATA_HOME="$tmp/data" \
  XDG_STATE_HOME="$tmp/state" \
  XDG_CONFIG_HOME="$tmp/xdg-config" \
  XDG_CACHE_HOME="$tmp/cache" \
  QT_QPA_PLATFORM=offscreen \
  QT_QUICK_BACKEND=software \
  timeout -k 2 120 qs -p "$tmp/config/shell.qml" --no-color 2>&1 |
  grep -v -E 'WAYLAND_DISPLAY is present|actually running|--- WARNING|window masks|No PanelWindow backend' |
  tee "$tmp/qs.log" || true

if [[ -s $tmp/calls.log ]]; then
  echo "HARNESS unexpected calls:" >&2
  cat "$tmp/calls.log" >&2
  exit 1
fi
grep -q 'HARNESS ok' "$tmp/qs.log"
