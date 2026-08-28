import QtQuick

Item {
  property string title: ""
  property color color: "transparent"
  property size minimumSize: Qt.size(0, 0)
  property size maximumSize: Qt.size(0, 0)
  property var screen: null
  property bool mask: false
}
