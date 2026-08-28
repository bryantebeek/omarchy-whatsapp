import QtQuick

Item {
  property bool opened: false
  property string message: ""
  property string cancelText: "Cancel"
  property string confirmText: "Confirm"
  property int selectedIndex: 1
  property color background: "black"
  property color foreground: "white"
  property color selectedText: "green"
  signal canceled()
  signal confirmed()
  function handleKey() { return false }
}
