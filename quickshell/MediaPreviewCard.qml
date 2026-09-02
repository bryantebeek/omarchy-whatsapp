import QtQuick
import QtQuick.Effects
import QtMultimedia
import qs.Commons

// Image and video previews. Videos always show their cached thumbnail until
// the panel's single player is handed this card's surface, so only one clip
// decodes at a time.
Item {
  id: root

  property var panel: null
  property var service: null
  property var player: null
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
  property string fontFamily: Style.font.family
  property color foreground: Color.foreground
  property color muted: Color.muted
  property color accent: Color.accent
  property alias videoSurface: inlineVideoOutput
  readonly property bool isImage: media && media.kind === "image"
  readonly property bool isVideo: media && media.kind === "video"
  readonly property bool isGif: isVideo && media.gif_playback === true
  readonly property real topMargin: Style.space(8)
  readonly property bool downloaded: media ? media.downloaded === true : false
  readonly property string mediaPath: media ? String(media.path || "") : ""
  readonly property string thumbnailPath: media
    ? String(media.thumbnail_path || "") : ""
  readonly property string displayPath: isVideo
    ? thumbnailPath : (downloaded ? mediaPath : thumbnailPath)
  // The decoded image is authoritative once it exists; the announced
  // dimensions only have to carry the layout until then.
  readonly property real imageAspectRatio: mediaPreviewImage.status === Image.Ready
    && mediaPreviewImage.sourceSize.width > 0
    && mediaPreviewImage.sourceSize.height > 0
    ? mediaPreviewImage.sourceSize.width / mediaPreviewImage.sourceSize.height
    : mediaAspectRatio
  readonly property bool inlineActive: panel
    && panel.activeInlineVideoCard === root
  readonly property bool inlinePlaying: inlineActive && player
    && player.playbackState === MediaPlayer.PlayingState

  visible: media && (media.kind === "image" || media.kind === "video")
  height: visible && media
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
    visible: !root.inlineActive
    source: root.visible && root.service
      ? root.service.fileUrl(root.displayPath,
        root.service.messageMediaRevision(root.message)) : ""
    asynchronous: true
    cache: false
    fillMode: Image.PreserveAspectFit
    layer.enabled: root.isImage
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

  VideoOutput {
    id: inlineVideoOutput
    anchors.fill: parent
    anchors.topMargin: root.topMargin
    visible: root.inlineActive
    fillMode: VideoOutput.PreserveAspectFit
    endOfStreamPolicy: VideoOutput.KeepLastFrame
  }

  HoverHandler {
    id: mediaPreviewHover
  }

  MouseArea {
    anchors.fill: parent
    anchors.topMargin: root.topMargin
    enabled: root.downloaded
    cursorShape: enabled ? Qt.PointingHandCursor : Qt.ArrowCursor
    onClicked: {
      if (!root.panel) return
      if (root.isVideo) root.panel.toggleInlineVideo(root)
      else if (root.service)
        root.panel.openImagePreview(root.mediaPath,
          root.service.messageMediaRevision(root.message))
    }
  }

  Text {
    anchors.centerIn: parent
    anchors.verticalCenterOffset: root.topMargin / 2
    visible: root.isVideo && !root.inlineActive
      && mediaPreviewImage.status !== Image.Ready
    text: "󰕧"
    color: root.muted
    font.family: root.fontFamily
    font.pixelSize: Style.font.displayLarge
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
      : (root.isVideo && root.downloaded
        ? (root.inlinePlaying ? "󰏤" : "󰐊")
        : "󰇚")
    tooltipText: downloading
      ? "Downloading media"
      : (root.isVideo
        ? (root.downloaded
          ? (root.inlinePlaying
            ? (root.isGif ? "Pause GIF" : "Pause video")
            : (root.isGif ? "Play GIF" : "Play video"))
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
      if (root.isVideo && root.downloaded) root.panel.toggleInlineVideo(root)
      else root.panel.downloadMedia(root.message, root.delegateItem)
    }
  }

  Component.onDestruction: if (panel
    && typeof panel.stopInlineVideo === "function")
    panel.stopInlineVideo(root)
}
