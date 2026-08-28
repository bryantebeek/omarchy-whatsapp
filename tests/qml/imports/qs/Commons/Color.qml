pragma Singleton

import QtQuick

QtObject {
  property color foreground: "#eeeeee"
  property color background: "#111111"
  property color accent: "#25d366"
  property color muted: "#777777"
  property color urgent: "#ff5555"
  readonly property QtObject popups: QtObject {
    property color background: "#222222"
    property color text: "#eeeeee"
    property color border: "#555555"
  }
}
