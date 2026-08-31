import QtQuick

QtObject {
  id: root

  property var command: []
  property bool running: false
  property var stdout: null
  property var stderr: null
  signal exited(int exitCode, int exitStatus)

  onRunningChanged: {
    if (running) {
      TestIo.processStarts = TestIo.processStarts.concat([command])
      TestIo.processes = TestIo.processes.concat([root])
    }
  }

  function emitStdout(line) {
    if (stdout) stdout.read(String(line))
  }

  function emitStderr(line) {
    if (stderr) stderr.read(String(line))
  }

  function finish(exitCode) {
    running = false
    exited(Number(exitCode || 0), 0)
  }
}
