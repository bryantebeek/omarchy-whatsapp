import QtQuick
import qs.Commons

Rectangle {
  id: root
  property var quote: null
  implicitHeight: quote ? content.implicitHeight + Style.space(12) : 0
  color: Qt.rgba(Color.accent.r, Color.accent.g, Color.accent.b, 0.12)
  radius: Style.cornerRadius
  Accessible.role: Accessible.StaticText
  Accessible.name: quote ? "Reply to " + author.text + ": " + preview.text : ""

  Rectangle {
    width: Style.space(3)
    height: parent.height
    color: Color.accent
  }
  Column {
    id: content
    anchors.left: parent.left
    anchors.right: parent.right
    anchors.verticalCenter: parent.verticalCenter
    anchors.margins: Style.space(8)
    Text {
      id: author
      width: parent.width
      text: root.quote ? String(root.quote.sender_name || root.quote.sender_jid || "Unknown sender") : ""
      textFormat: Text.PlainText
      color: Color.accent
      font.family: Style.font.family
      font.pixelSize: Style.font.caption
      font.bold: true
      elide: Text.ElideRight
    }
    Text {
      id: preview
      width: parent.width
      text: root.quote ? String(root.quote.text || "Message") : ""
      textFormat: Text.PlainText
      color: Color.foreground
      font.family: Style.font.family
      font.pixelSize: Style.font.caption
      maximumLineCount: 2
      wrapMode: Text.Wrap
      elide: Text.ElideRight
    }
  }
}
