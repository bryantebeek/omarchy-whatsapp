pragma Singleton

import QtQuick

QtObject {
  property var socketWrites: []
  property var sockets: []
  property var processStarts: []
  property var processes: []
  property var files: []

  function reset() {
    socketWrites = []
    sockets = []
    processStarts = []
    processes = []
    files = []
  }
}
