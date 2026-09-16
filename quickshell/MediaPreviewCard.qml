import QtQuick
import QtQuick.Effects
import qs.Commons

import "Model.js" as Model

// Image and video thumbnails. Tapping a downloaded clip opens the panel's
// full-size viewer, which owns the single shared player, so only one clip
// ever decodes at a time.
Item {
  id: root

  property var panel: null
  property var service: null
  property var message: null
  property var media: null
  // Anchoring the viewport across a download needs the delegate that owns
  // this card, not the card itself.
  property var delegateItem: null
  property real maskRadius: 0
  property real mediaAspectRatio: 1
  property real devicePixelRatio: 1
  property string maskObjectName: ""
  property string imageObjectName: ""
  property string downloadButtonObjectName: ""
  property string hdBadgeObjectName: ""
  property string fontFamily: Style.font.family
  property color foreground: Color.foreground
  property color muted: Color.muted
  property color accent: Color.accent
  readonly property bool isImage: media && media.kind === "image"
  readonly property bool isVideo: media && media.kind === "video"
  readonly property bool isGif: isVideo && media.gif_playback === true
  readonly property bool showHdBadge: Model.isHighDefinitionImage(media)
  property real topMargin: Style.space(8)
  readonly property bool downloaded: media ? media.downloaded === true : false
  readonly property string mediaPath: media ? String(media.path || "") : ""
  readonly property string thumbnailPath: media
    ? String(media.thumbnail_path || "") : ""
  readonly property string displayPath: Model.previewDisplayPath(media)
  // The decoded image is authoritative once it exists; the announced
  // dimensions only have to carry the layout until then.
  readonly property real imageAspectRatio: mediaPreviewImage.status === Image.Ready
    && mediaPreviewImage.sourceSize.width > 0
    && mediaPreviewImage.sourceSize.height > 0
    ? mediaPreviewImage.sourceSize.width / mediaPreviewImage.sourceSize.height
    : mediaAspectRatio

  function openPreview() {
    if (!panel) return
    if (isVideo) panel.openVideoPreview(mediaPath, isGif)
    else if (service && media) panel.openImagePreview(mediaPath,
      service.messageMediaRevision(message))
  }

  visible: media && (media.kind === "image" || media.kind === "video")
  // Height must not read visible: visibility is effective, so the card would
  // collapse to zero whenever any ancestor is hidden (or never shown, as in
  // tests) and corrupt the delegate layout built on top of it.
  height: media
    ? topMargin + width / (isVideo ? mediaAspectRatio : imageAspectRatio) : 0

  Rectangle {
    id: mediaPreviewMask
    objectName: root.maskObjectName
    anchors.fill: parent
    anchors.topMargin: root.topMargin
    radius: root.maskRadius
    visible: false
    layer.enabled: true
  }

  Image {
    id: mediaPreviewImage
    objectName: root.imageObjectName
    anchors.fill: parent
    anchors.topMargin: root.topMargin
    source: root.visible && root.service
      ? root.service.fileUrl(root.displayPath,
        root.service.messageMediaRevision(root.message)) : ""
    asynchronous: true
    cache: false
    fillMode: Image.PreserveAspectFit
    layer.enabled: true
    layer.smooth: true
    layer.effect: MultiEffect {
      maskEnabled: true
      maskSource: mediaPreviewMask
      maskThresholdMin: 0.5
      maskSpreadAtMin: 1.0
    }
    onStatusChanged: {
      if (status === Image.Ready && root.panel)
        root.panel.scheduleMediaDownloadAnchorRestore(
          root.message ? root.message.id : "")
    }
  }

  HoverHandler {
    id: mediaPreviewHover
  }

  MouseArea {
    anchors.fill: parent
    anchors.topMargin: root.topMargin
    enabled: root.downloaded
    cursorShape: enabled ? Qt.PointingHandCursor : Qt.ArrowCursor
    onClicked: root.openPreview()
  }

  Text {
    anchors.centerIn: parent
    anchors.verticalCenterOffset: root.topMargin / 2
    visible: root.isVideo
      && mediaPreviewImage.status !== Image.Ready
    text: "󰕧"
    color: root.muted
    font.family: root.fontFamily
    font.pixelSize: Style.font.displayLarge
  }

  Rectangle {
    objectName: root.hdBadgeObjectName
    anchors.left: parent.left
    anchors.top: parent.top
    anchors.leftMargin: Style.space(6)
    anchors.topMargin: root.topMargin + Style.space(6)
    visible: root.showHdBadge
    width: hdBadgeLabel.implicitWidth + Style.space(10)
    height: hdBadgeLabel.implicitHeight + Style.space(4)
    radius: Style.space(4)
    color: Qt.rgba(0, 0, 0, 0.65)

    Text {
      id: hdBadgeLabel
      anchors.centerIn: parent
      text: "HD"
      color: "white"
      font.family: root.fontFamily
      font.pixelSize: Style.font.caption
      font.bold: true
    }
  }

  DevicePixelButton {
    objectName: root.downloadButtonObjectName
    readonly property bool downloading: visible && root.service
      && root.service.mediaDownloading(root.message)

    anchors.centerIn: parent
    anchors.verticalCenterOffset: root.topMargin / 2
    visible: root.media && root.visible
      && (root.isVideo || !root.downloaded)
    opacity: root.isVideo && root.downloaded
      ? (mediaPreviewHover.hovered ? 1 : 0) : 1
    width: Style.space(40)
    height: Style.space(40)
    iconSize: Style.font.icon * 1.5
    iconText: downloading ? "󰔟"
      : (root.isVideo && root.downloaded ? "󰐊" : "󰇚")
    tooltipText: downloading
      ? "Downloading media"
      : (root.isVideo
        ? (root.downloaded
          ? (root.isGif ? "Play GIF" : "Play video")
          : (root.isGif ? "Download GIF" : "Download video"))
        : "Download full image")
    foreground: root.foreground
    accent: root.accent
    devicePixelRatio: root.devicePixelRatio
    enabled: visible && root.service && !downloading
      && (!root.isVideo || !root.downloaded || mediaPreviewHover.hovered)

    Behavior on opacity {
      NumberAnimation {
        duration: 140
        easing.type: Easing.OutCubic
      }
    }

    onClicked: {
      if (!root.panel) return
      if (root.isVideo && root.downloaded) root.openPreview()
      else root.panel.downloadMedia(root.message, root.delegateItem)
    }
  }
}
