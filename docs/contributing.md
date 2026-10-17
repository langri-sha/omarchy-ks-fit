# Contributing

Notes for working on the plugin. They live here rather than in an `AGENTS.md`:
the repository is installed as is, and the plugin marketplace's maintainers ask
for agent-instruction files to be taken out of plugin trees.

## Who owns which file

Projen owns every file listed in `.projen/files.json` and
`daemon/.projen/files.json`: among them `manifest.json`, `shell/Version.js`,
`package.json`, the `Cargo.toml` files, the toolchain, formatter, editor, lint
and Renovate configuration, `.husky/`, the ignore and attribute files,
`CODEOWNERS` and `license`. Everything else is yours.

Synthesized files are read-only; change them in `.projenrc.ts`. The version
lives there as well: bump it and synthesize, and the manifest,
`shell/Version.js` and the crate move together. The helper is then rebuilt from
the panel.

`shell/Model.js` and `shell/Version.js` start with QML's `.pragma library`,
which Prettier can't parse, so both are left out of formatting. Keep `Model.js`
in Prettier's style by hand, or format it without its first line.

## Checks

```sh
pnpm test             # cargo test, then the Model.js tests with node --test
pnpm qmllint          # Qt 6 qmllint against the installed shell
pnpm plugin:render    # render offscreen against the mock helper
pnpm plugin:validate  # omarchy-plugin-validate on a staged copy of the tree
cargo clippy --locked --all-targets -- -D warnings
```

- Don't run `omarchy plugin validate .` in a worktree: pnpm's `node_modules` is
  made of symlinks, which validation rejects. The staged copy holds what
  `omarchy plugin add` would clone.
- `pnpm qmllint` allows only the warning categories the shell's own plugins
  produce, plus `uncreatable-type` for the window in `ServiceWindows.qml`.
- `pnpm plugin:render` runs a separate offscreen `qs` instance on the installed
  Omarchy `Ui` and `Commons`, never the live shell, and writes PNGs to
  `target/render`. It starts the real `Service.qml` with no helper, grabs the
  build banner, places `tests/render/mock-ks-fit` where the launcher looks and
  retries through a held lock, then walks through scanning, connecting, every
  period and metric, a walk, a stale-QML update, Bluetooth off, a pad out of
  range, a link lost mid-walk and demo mode. Along the way it checks that a
  hovered Start doesn't start the belt, that arrow keys reach it, that
  `Backspace` stops it, that a late click on Stop can't land on Start, that a
  demo switch and Restart shell wait through the lost link, that a helper being
  restarted takes no commands, and that a stop key with the belt at rest raises
  no alarm. It prints `HARNESS ok` at the end. Desktop commands such as
  `omarchy-notification-send` are stubbed on `PATH`, and a call to any of them
  fails the run.
- `KS_FIT_RENDER_HELPER=target/debug/ks-fit pnpm plugin:render` drives the real
  helper instead, in demo mode and with its data and `XDG_RUNTIME_DIR` under a
  temp dir, so its lock can't collide with a live shell's; the steps that need
  the mock's `{"cmd":"mock",…}` controls are skipped.

## How it fits together

- `daemon/` (crate `ks-fit`) owns the Bluetooth link, the belt and the database,
  and speaks JSON lines over stdio: one command per line in, one message per
  line out.
- `shell/Service.qml` is a singleton the shell keeps across plugin reloads
  (`keepLoaded`). It runs the helper through `bin/ks-fit-launcher`, holds its
  latest `state` and the stats replies, and serves the
  `io.github.langri-sha.ks-fit` IPC target. The bar widgets, one per monitor,
  and the panel only bind to it. It's a `QtObject`, so `state` doesn't shadow
  `Item.state`; its timers, process and IPC handler are property values.
- The service restarts the helper by closing its stdin and waiting up to 3 s
  before a SIGTERM, which the launcher forwards; meanwhile `send()` refuses, and
  IPC answers `helper-starting`. A fatal `already-running` means the previous
  helper is still shutting down, and is retried with the usual backoff, from
  `missing` and `outdated` as well. `missing`, `outdated` and `failed` wait for
  **Retry** or for `bin/ks-fit-build`, which calls `retryHelper` when it's done.
- `shell/ServiceWindows.qml` holds an invisible layer surface whose
  `IdleInhibitor` keeps the screen awake while the belt may be moving. It's
  enabled only after the surface maps, because Hyprland ignores one that is
  already enabled. `Service.qml` must not mention `PanelWindow`, or it would not
  load offscreen.
- `shell/PanelContent.qml` is the panel body and its cursor model, kept apart
  from `Panel.qml` so it renders offscreen. The helper banner sits above the
  stats or workout view and never replaces it: a kept helper may still be
  driving a belt after an update.
- `bin/ks-fit-launcher` runs the helper as its own child, so the helper reads
  EOF when the shell stops the process, and prunes helpers built for other
  versions once the current one is up. `bin/ks-fit-build` builds the helper
  outside the plugin directory, because the shell reloads every plugin on any
  write under `~/.config/omarchy/plugins`.

## Trying a change in the shell

```sh
bin/ks-fit-build
ln -s "$PWD" ~/.config/omarchy/plugins/io.github.langri-sha.ks-fit
omarchy-shell shell rescanPlugins
omarchy plugin enable io.github.langri-sha.ks-fit
```

Set `demo` to `true` first to try it without a pad. A symlinked plugin isn't
hot-reloaded: run `omarchy-shell shell rescanPlugins` after changing the widget
or the panel, and `omarchy-restart-shell` after changing `Service.qml`, which
the shell keeps loaded. `omarchy plugin remove io.github.langri-sha.ks-fit`
unlinks it again.

## Marketplace listing

The marketplace refuses a listing with a symlink anywhere in the tree or a
second `manifest.json` one directory down.

Its security baseline also reads the code, readme and configuration files
outside `.github/`, `docs/` and the test directories, and asks for a maintainer
review when it finds an installer-like file name (`install`, `setup`,
`uninstall`) or commands such as `sudo`, `systemctl`, `cargo install`,
`pacman -S`, `curl`, `wget`, the coreutils `install` or pnpm's spelled-out
install command (`pnpm i` passes). Keep those out so the baseline stays clean.

Because the helper has to be built before the plugin works, a maintainer may
list it as "Manual setup".
