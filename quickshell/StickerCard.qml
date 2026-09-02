import QtQuick
import qs.Commons

// Stickers render from the private media cache. Lottie stickers keep their
// embedded raster preview because Qt Lottie trusts its input while message
// content is sender-controlled.
Item {
  id: root

  property var panel: null
  property var service: null
  property var message: null
  property var media: null
  property bool active: false
  property real aspectRatio: 1
  property string statusObjectName: ""
  property string imageObjectName: ""
  property string fontFamily: Style.font.family
  property real metaFontSize: Style.font.caption
  property color foreground: Color.foreground
  property color muted: Color.muted
  readonly property bool downloaded: media ? media.downloaded === true : false
  readonly property bool lottie: media ? media.lottie === true : false
  readonly property string mediaPath: media ? String(media.path || "") : ""
  readonly property string thumbnailPath: media
    ? String(media.thumbnail_path || "") : ""
  readonly property string displayPath: downloaded ? mediaPath : thumbnailPath

  visible: active
  height: active ? width / aspectRatio : 0

  AnimatedImage {
    id: stickerImage
    objectName: root.imageObjectName
    anchors.fill: parent
    source: root.active && root.service
      ? root.service.fileUrl(root.displayPath,
        root.service.messageMediaRevision(root.message)) : ""
    asynchronous: true
    cache: false
    fillMode: Image.PreserveAspectFit
    onStatusChanged: {
      if (status === AnimatedImage.Ready && root.panel)
        root.panel.scheduleMediaDownloadAnchorRestore(
          root.message ? root.message.id : "")
    }
  }

  Text {
    anchors.centerIn: parent
    width: parent.width
    visible: String(stickerImage.source) === ""
      || stickerImage.status === AnimatedImage.Error
    text: root.media && root.media.accessibility_label
      ? String(root.media.accessibility_label)
      : (root.lottie ? "Lottie sticker unavailable" : "Sticker unavailable")
    color: root.muted
    font.family: root.fontFamily
    font.pixelSize: Style.font.caption
    wrapMode: Text.WrapAtWordBoundaryOrAnywhere
    horizontalAlignment: Text.AlignHCenter
  }

  Text {
    anchors.right: parent.right
    anchors.bottom: parent.bottom
    anchors.margins: Style.space(4)
    visible: root.lottie && stickerImage.status === AnimatedImage.Ready
    text: "Animated sticker"
    color: root.foreground
    font.family: root.fontFamily
    font.pixelSize: root.metaFontSize
  }

  Text {
    objectName: root.statusObjectName
    readonly property bool active: root.active && !root.lottie
      && !root.downloaded && root.service
      && root.service.mediaDownloading(root.message)
    anchors.centerIn: parent
    visible: active
    text: "󰔟"
    color: root.foreground
    font.family: root.fontFamily
    font.pixelSize: Style.font.icon * 1.5
  }
}
