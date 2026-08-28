import QtQuick

QtObject {
  id: root

  property string path: ""
  property bool watchChanges: false
  property bool atomicWrites: false
  property bool printErrors: true
  property string contents: ""

  signal loaded()
  signal loadFailed()
  signal saved()
  signal saveFailed()

  function text() { return contents }

  function setText(value) {
    contents = String(value)
    if (TestIo.files.indexOf(root) < 0)
      TestIo.files = TestIo.files.concat([root])
  }

  function reload() {}
}
