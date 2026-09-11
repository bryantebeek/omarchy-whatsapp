import QtQuick

Item {
  enum FillMode { Stretch, PreserveAspectFit, PreserveAspectCrop }
  enum EndOfStreamPolicy { ClearAtEnd, KeepLastFrame }

  property int fillMode: VideoOutput.PreserveAspectFit
  property int endOfStreamPolicy: VideoOutput.ClearAtEnd
  readonly property rect contentRect: Qt.rect(0, 0, width, height)
}
