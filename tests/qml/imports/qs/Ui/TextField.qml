import QtQuick
import QtQuick.Controls as QQC

QQC.TextField {
  property color foreground: "white"
  property color accent: "green"
  property bool _focused: activeFocus
  property bool _hot: hovered
  property var _borderSpec: ({})
}
