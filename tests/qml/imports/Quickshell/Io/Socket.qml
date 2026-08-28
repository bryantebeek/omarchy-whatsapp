import QtQuick

QtObject {
  id: root

  property string path: ""
  property bool connected: false
  property var parser: null

  signal connectionStateChanged()
  signal error(var value)

  onConnectedChanged: connectionStateChanged()

  function write(value) {
    TestIo.socketWrites = TestIo.socketWrites.concat([String(value)])
  }

  function flush() {}

  Component.onCompleted: {
    TestIo.sockets = TestIo.sockets.concat([root])
    connectionStateChanged()
  }
}
