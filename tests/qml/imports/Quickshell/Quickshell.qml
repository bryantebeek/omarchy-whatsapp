pragma Singleton

import QtQuick

QtObject {
  property var detachedCommands: []

  function env(name) {
    if (name === "XDG_RUNTIME_DIR") return "/tmp/omarchy-whatsapp-qml-tests/runtime"
    if (name === "XDG_STATE_HOME") return "/tmp/omarchy-whatsapp-qml-tests/state"
    if (name === "HOME") return "/tmp/omarchy-whatsapp-qml-tests/home"
    return ""
  }

  function execDetached(command) {
    detachedCommands = detachedCommands.concat([command])
  }

  function reset() {
    detachedCommands = []
  }
}
