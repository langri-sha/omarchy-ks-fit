import QtQuick

// Headless singleton the shell creates once and keeps across plugin reloads
// (manifest keepLoaded). It will own the helper process and all state.
QtObject {
  id: root

  property var shell: null
  property var manifest: null
}
