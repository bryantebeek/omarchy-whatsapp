import QtQuick

Item {
  property real colorization: 0
  property color colorizationColor: "transparent"
  property bool maskEnabled: false
  property var maskSource: null
  property real maskThresholdMin: 0
  property real maskSpreadAtMin: 0
}
