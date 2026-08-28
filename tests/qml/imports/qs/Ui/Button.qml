import QtQuick

Item {
  property string text: ""
  property string iconText: ""
  property string tooltipText: ""
  property color foreground: "white"
  property color background: "transparent"
  property color accent: "green"
  property bool bordered: false
  property bool active: false
  property bool selected: false
  property bool focusable: false
  property bool destructive: false
  property string fontFamily: "Sans"
  property int fontSize: 14
  property int iconSize: 18
  property real verticalPadding: 4
  property real horizontalPadding: 6
  property real radius: 0
  property var borderSpec: ({})
  property var _borderSpec: ({})
  property color _selectedColor: accent
  property real _reservedBorderTop: 0
  property real _reservedBorderBottom: 0
  property real _reservedContentLeftInset: 0
  signal clicked()
  implicitWidth: 32
  implicitHeight: 32
  function click() { clicked() }
}
