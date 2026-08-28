import QtQuick

Item {
  property QtObject bar: null
  property Component iconComponent: null
  property real slotSize: 0
  property bool active: false
  property bool useActiveColor: false
  property bool dimmed: false
  property color foreground: "white"
  property string tooltipText: ""
  signal pressed(int buttonCode)
}
