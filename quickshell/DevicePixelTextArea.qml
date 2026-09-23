import QtQuick
import QtQuick.Controls as QQC
import qs.Commons

// Wrapping counterpart of DevicePixelTextField with the same kit styling.
// Enter emits accepted() like a TextField; Shift+Enter inserts a newline.
QQC.TextArea {
  id: root

  property real devicePixelRatio: 1
  property color foreground: Color.foreground
  property color accent: Color.accent

  readonly property real singleLineHeight: implicitHeight - contentHeight
    + contentHeight / Math.max(1, lineCount)
  readonly property var _borderSpec: Border.controlSpec(activeFocus
    ? "focus" : (hovered ? "hover-cursor" : "normal"), foreground, accent)

  signal accepted()

  function handleReturn(event) {
    if (event.modifiers & Qt.ShiftModifier) event.accepted = false
    else accepted()
  }

  wrapMode: TextEdit.Wrap
  verticalAlignment: TextEdit.AlignVCenter
  font.family: Style.font.family
  font.pixelSize: Style.font.body
  color: foreground
  selectionColor: Style.selectionFillFor(foreground, accent)
  selectedTextColor: foreground
  placeholderTextColor: Qt.darker(foreground, 1.6)
  leftPadding: Style.spacing.controlPaddingX + Border.left(_borderSpec)
  rightPadding: Style.spacing.controlPaddingX + Border.right(_borderSpec)
  topPadding: Style.spacing.inputPaddingY + Border.top(_borderSpec)
  bottomPadding: Style.spacing.inputPaddingY + Border.bottom(_borderSpec)

  Keys.onReturnPressed: function(event) { root.handleReturn(event) }
  Keys.onEnterPressed: function(event) { root.handleReturn(event) }

  background: DevicePixelBorderSurface {
    color: Style.controlFill(root.activeFocus, root.hovered, root.foreground,
      root.accent)
    sourceBorderSpec: root._borderSpec
    devicePixelRatio: root.devicePixelRatio
    radius: Style.cornerRadius
  }
}
