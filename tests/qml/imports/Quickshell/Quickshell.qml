pragma Singleton

import QtQuick

QtObject {
  property var detachedCommands: []
  property var screens: [{ width: 1920, height: 1080, name: "mock" }]

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
