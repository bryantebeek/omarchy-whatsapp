pragma Singleton

import QtQuick

QtObject {
  property int cornerRadius: 8
  property real normalBorderWidth: 1
  readonly property QtObject font: QtObject {
    property string family: "Sans"
    property int caption: 12
    property int bodySmall: 12
    property int body: 14
    property int subtitle: 16
    property int title: 18
    property int display: 24
    property int displayLarge: 32
    property int icon: 18
    property int iconLarge: 24
  }
  readonly property QtObject spacing: QtObject {
    property real controlGap: 6
    property real controlPaddingX: 6
    property real controlPaddingY: 4
  }
  readonly property QtObject bar: QtObject { property real statusSlot: 26 }

  function space(value) { return Number(value) }
  function controlFill() { return "transparent" }
  function hoverFillFor() { return "#222222" }
  function normalFillFor() { return "transparent" }
  function selectedFillFor() { return "#333333" }
  function normalBorderFor() { return Border.none() }
}
