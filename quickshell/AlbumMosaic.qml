import QtQuick
import QtQuick.Effects
import qs.Commons

import "Model.js" as Model

// Mosaic for consecutive uncaptioned photos and clips from one sender. Two
// tiles sit side by side, three stack two beside one tall tile, and four form
// a grid. Tiles center-crop to uniform cells; tapping one opens or downloads
// that message's media.
Item {
  id: root

  property var panel: null
  property var service: null
  // Assign real JS arrays via bindings or post-creation assignment: arrays
  // passed as creation properties lose arrayness crossing the QML/C++
  // boundary and read back with totalCount 0.
  property var messages: []
  property var delegateItem: null
  property real maskRadius: 0
  property real devicePixelRatio: 1
  property string fontFamily: Style.font.family
  property color foreground: Color.foreground
  property color accent: Color.accent
  readonly property int totalCount: Array.isArray(messages) ? messages.length : 0
  readonly property real gap: Style.space(2)
  readonly property real cellSize: (width - gap) / 2
  readonly property bool showMosaic: totalCount > 1

  function tileMessage(index) {
    return index >= 0 && index < totalCount ? messages[index] : null
  }

  function tileMedia(message) {
    if (!message) return null
    if (service && typeof service.messageMedia === "function")
      return service.messageMedia(message)
    return message.media || null
  }

  function openTile(message) {
    if (!panel || !message) return
    var media = tileMedia(message)
    if (!media) return
    if (media.downloaded !== true) {
      panel.downloadMedia(message, delegateItem)
      return
    }
    if (media.kind === "video")
      panel.openVideoPreview(String(media.path || ""),
        media.gif_playback === true)
    else if (service)
      panel.openImagePreview(String(media.path || ""),
        service.messageMediaRevision(message))
  }

  function tileX(index) {
    if (totalCount === 3) return index === 0 ? 0 : cellSize + gap
    return index % 2 === 0 ? 0 : cellSize + gap
  }

  function tileY(index) {
    if (totalCount === 3)
      return index === 0 ? 0 : (index - 1) * (cellSize + gap)
    return index < 2 ? 0 : cellSize + gap
  }

  function tileHeight(index) {
    return totalCount === 3 && index === 0
      ? cellSize * 2 + gap : cellSize
  }

  visible: showMosaic
  height: !showMosaic ? 0
    : (totalCount === 2 ? cellSize : cellSize * 2 + gap)
  layer.enabled: true
  layer.smooth: true
  layer.effect: MultiEffect {
    maskEnabled: true
    maskSource: mosaicMask
    maskThresholdMin: 0.5
    maskSpreadAtMin: 1.0
  }

  Rectangle {
    id: mosaicMask
    anchors.fill: parent
    radius: root.maskRadius
    visible: false
    layer.enabled: true
  }

  Repeater {
    model: root.totalCount
    delegate: Item {
      required property int index

      readonly property var memberMessage: root.tileMessage(index)
      readonly property var tileMedia: root.tileMedia(memberMessage)
      readonly property string memberId: memberMessage
        ? String(memberMessage.id || "") : ""
      readonly property bool tileDownloaded: tileMedia
        ? tileMedia.downloaded === true : false
      readonly property bool tileIsVideo: tileMedia
        ? tileMedia.kind === "video" : false
      readonly property bool showHdBadge:
        Model.isHighDefinitionImage(tileMedia)
      readonly property bool showDownloadButton: !tileDownloaded

      objectName: "albumTile-" + memberId
      x: root.tileX(index)
      y: root.tileY(index)
      width: root.cellSize
      height: root.tileHeight(index)

      Image {
        anchors.fill: parent
        source: root.visible && root.service && tileMedia
          ? root.service.fileUrl(Model.previewDisplayPath(tileMedia),
            root.service.messageMediaRevision(memberMessage)) : ""
        asynchronous: true
        cache: false
        fillMode: Image.PreserveAspectCrop
        onStatusChanged: {
          if (status === Image.Ready && root.panel)
            root.panel.scheduleMediaDownloadAnchorRestore(memberId)
        }
      }

      Text {
        anchors.centerIn: parent
        visible: tileDownloaded && tileIsVideo
        text: "󰐊"
        color: "white"
        font.family: root.fontFamily
        font.pixelSize: Style.font.icon * 1.5
      }

      Rectangle {
        objectName: "albumHdBadge-" + memberId
        anchors.left: parent.left
        anchors.top: parent.top
        anchors.margins: Style.space(6)
        visible: showHdBadge
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

      MouseArea {
        anchors.fill: parent
        onClicked: root.openTile(memberMessage)
      }

      DevicePixelButton {
        objectName: "albumDownloadButton-" + memberId
        readonly property bool downloading: visible && root.service
          && root.service.mediaDownloading(memberMessage)

        anchors.centerIn: parent
        visible: showDownloadButton
        width: Style.space(40)
        height: Style.space(40)
        iconSize: Style.font.icon * 1.5
        iconText: downloading ? "󰔟" : "󰇚"
        tooltipText: downloading ? "Downloading media" : "Download media"
        foreground: root.foreground
        accent: root.accent
        devicePixelRatio: root.devicePixelRatio
        enabled: visible && root.service && !downloading

        onClicked: root.openTile(memberMessage)
      }
    }
  }
}
