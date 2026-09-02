import QtQuick
import qs.Commons

import "Model.js" as Model

// Static and final live-location snapshots. The app deliberately hosts no map
// widget, so the cached thumbnail is the whole preview and the desktop's map
// handler owns the interactive view.
Rectangle {
  id: root

  // Reaches the panel's shared clock and label helpers explicitly instead of
  // relying on an implicit outer root reference.
  property var panel: null
  property var service: null
  property var media: null
  property real messageTimestamp: 0
  property string messageTimeFormat: "HH:mm"
  property real devicePixelRatio: 1
  property string fontFamily: Style.font.family
  property color foreground: Color.foreground
  property color background: Color.background
  property color muted: Color.muted
  property color accent: Color.accent
  // WhatsApp stops sending updates at the end of a live share. When the
  // explicit deadline is missing it is reconstructed from the last update and
  // the announced duration.
  readonly property bool live: media !== null && media !== undefined
    && media.live === true
  readonly property real liveUntil: {
    if (!live) return 0
    var exact = Number(media.live_until || 0)
    if (exact > 0) return exact
    var started = Number(media.updated_at || messageTimestamp || 0)
    var duration = Number(media.duration_seconds || 0)
    return started > 0 ? started + duration : 0
  }
  readonly property alias openMapButton: locationOpenButton

  visible: media && media.kind === "location"
  height: visible ? Style.space(120) : 0
  radius: 0
  clip: true
  color: Style.normalFillFor(foreground, accent)

  Image {
    anchors.fill: parent
    source: root.visible && root.service && root.media
      && root.media.thumbnail_path
      ? root.service.fileUrl(root.media.thumbnail_path)
        + "?revision=" + String(root.media.updated_at || 0) : ""
    asynchronous: true
    cache: false
    fillMode: Image.PreserveAspectCrop
    smooth: true
    mipmap: true
    opacity: status === Image.Ready ? 1 : 0
  }

  Rectangle {
    anchors.left: parent.left
    anchors.right: parent.right
    anchors.bottom: parent.bottom
    height: Style.space(72)
    gradient: Gradient {
      GradientStop {
        position: 0
        color: "transparent"
      }
      GradientStop {
        position: 1
        color: Qt.rgba(root.background.r, root.background.g,
          root.background.b, 0.9)
      }
    }
  }

  HoverHandler {
    id: locationHover
  }

  Row {
    anchors.left: parent.left
    anchors.bottom: parent.bottom
    anchors.margins: Style.space(10)
    spacing: Style.space(7)
    width: Math.max(0, parent.width - Style.space(20)
      - (liveRemainingLabel.visible
        ? liveRemainingLabel.width + Style.space(10) : 0))

    Text {
      anchors.bottom: parent.bottom
      text: "󰍎"
      color: root.foreground
      font.family: root.fontFamily
      font.pixelSize: Style.font.icon
    }

    Column {
      anchors.bottom: parent.bottom
      width: parent.width - Style.space(7) - Style.font.icon
      spacing: Style.space(1)

      Text {
        objectName: "locationName"
        width: parent.width
        text: String(root.media
          ? (root.media.name || root.media.address)
            || (root.media.live ? "Live location" : "Location")
          : "Location")
        color: root.foreground
        font.family: root.fontFamily
        font.pixelSize: Style.font.body
        font.bold: true
        elide: Text.ElideRight
      }
      Text {
        objectName: "locationAddress"
        visible: root.media && String(root.media.name || "").length > 0
          && String(root.media.address || "").length > 0
        width: parent.width
        text: root.media ? String(root.media.address || "") : ""
        color: root.muted
        font.family: root.fontFamily
        font.pixelSize: Style.font.caption
        elide: Text.ElideRight
      }
    }
  }

  Text {
    id: liveRemainingLabel
    objectName: "locationLiveRemaining"
    visible: root.live
    anchors.right: parent.right
    anchors.bottom: parent.bottom
    anchors.margins: Style.space(10)
    text: !root.live || !root.panel ? ""
      : root.liveUntil > 0
        ? root.panel.remainingTimeLabel(root.liveUntil)
        : "Updated " + Model.messageTime(root.media.updated_at,
          root.messageTimeFormat)
    color: root.foreground
    font.family: root.fontFamily
    font.pixelSize: Style.font.caption
    font.bold: true
  }

  MouseArea {
    anchors.fill: parent
    cursorShape: Qt.PointingHandCursor
    z: 1
    onClicked: if (root.service)
      root.service.openMap(root.media.latitude_e7, root.media.longitude_e7)
  }

  DevicePixelButton {
    id: locationOpenButton
    anchors.centerIn: parent
    visible: root.visible
    opacity: locationHover.hovered ? 1 : 0
    width: Style.space(40)
    height: Style.space(40)
    iconSize: Style.font.icon * 1.5
    iconText: "󰏌"
    tooltipText: "Open map"
    foreground: root.foreground
    accent: root.accent
    enabled: locationHover.hovered && root.service
    devicePixelRatio: root.devicePixelRatio
    z: 2

    Behavior on opacity {
      NumberAnimation {
        duration: 140
        easing.type: Easing.OutCubic
      }
    }

    onClicked: root.service.openMap(root.media.latitude_e7,
      root.media.longitude_e7)
  }
}
