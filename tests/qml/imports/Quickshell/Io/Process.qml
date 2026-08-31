import QtQuick

QtObject {
  id: root

  property var command: []
  property bool running: false
  property var stderr: null
  property var stdout: null
  signal exited(int exitCode, int exitStatus)

  onRunningChanged: {
    if (running)
      TestIo.processStarts = TestIo.processStarts.concat([command])
  }
}
