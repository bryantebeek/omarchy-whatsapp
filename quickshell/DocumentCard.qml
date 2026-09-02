import QtQuick
import qs.Commons

import "Model.js" as Model

// Document attachments are never previewed inline: the daemon hands the shell
// a cached file that the desktop's own handler opens.
Item {
  id: root

  property var service: null
  property var media: null
  property real devicePixelRatio: 1
  property string fontFamily: Style.font.family
  property color foreground: Color.foreground
  property color muted: Color.muted
  property color accent: Color.accent
  readonly property alias openButton: documentOpenButton
  readonly property alias saveButton: documentSaveButton

  visible: media && media.kind === "document"
  height: visible ? Math.max(documentIcon.implicitHeight,
    documentTextColumn.implicitHeight, Style.space(34)) : 0

  Text {
    id: documentIcon
    anchors.left: parent.left
    anchors.verticalCenter: parent.verticalCenter
    text: "󰈙"
    color: root.muted
    font.family: root.fontFamily
    font.pixelSize: Style.font.displayLarge
  }
  Column {
    id: documentTextColumn
    anchors.left: documentIcon.right
    anchors.right: documentOpenButton.left
    anchors.leftMargin: Style.space(10)
    anchors.rightMargin: Style.space(8)
    anchors.verticalCenter: parent.verticalCenter
    spacing: Style.space(3)
    Text {
      width: parent.width
      text: String(root.media ? root.media.file_name || "Document" : "Document")
      color: root.foreground
      font.family: root.fontFamily
      font.pixelSize: Style.font.body
      font.bold: true
      elide: Text.ElideMiddle
    }
    Text {
      width: parent.width
      text: root.media
        ? Model.documentDetails(root.media.mime_type, root.media.file_size,
          root.media.page_count) : "Document"
      color: root.foreground
      font.family: root.fontFamily
      font.pixelSize: Style.font.caption
      elide: Text.ElideRight
    }
  }
  DevicePixelButton {
    id: documentOpenButton
    anchors.right: documentSaveButton.left
    anchors.rightMargin: Style.space(4)
    anchors.verticalCenter: parent.verticalCenter
    width: Style.space(34)
    height: Style.space(34)
    iconText: "󰏌"
    tooltipText: "Open document"
    foreground: root.foreground
    accent: root.accent
    bordered: false
    devicePixelRatio: root.devicePixelRatio
    onClicked: if (root.service) root.service.openFile(root.media.path)
  }
  DevicePixelButton {
    id: documentSaveButton
    anchors.right: parent.right
    anchors.verticalCenter: parent.verticalCenter
    width: Style.space(34)
    height: Style.space(34)
    iconText: "󰇚"
    tooltipText: "Save to Downloads"
    foreground: root.foreground
    accent: root.accent
    bordered: false
    devicePixelRatio: root.devicePixelRatio
    onClicked: if (root.service)
      root.service.saveFile(root.media.path, root.media.file_name)
  }
}
