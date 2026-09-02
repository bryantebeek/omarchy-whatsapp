import QtQuick
import QtMultimedia
import qs.Commons

import "Model.js" as Model

// Voice notes and shared audio share one row. Playback is owned by the panel's
// single player, so `panel.activeVoiceMessageCard` decides which card is live
// and only that card follows the player's position.
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
  property real devicePixelRatio: 1
  property string fontFamily: Style.font.family
  property real metaFontSize: Style.font.caption
  property color foreground: Color.foreground
  property color accent: Color.accent
  property color timestamp: Color.foreground
  readonly property alias playButton: voiceMessageButton
  readonly property bool downloaded: media ? media.downloaded === true : false
  readonly property string mediaPath: media ? String(media.path || "") : ""
  readonly property bool active: panel
    && panel.activeVoiceMessageCard === root
  readonly property bool playing: active && player
    && player.playbackState === MediaPlayer.PlayingState
  readonly property real totalSeconds: Math.max(0,
    Number(media ? media.duration_seconds || 0 : 0))
  readonly property real elapsedSeconds: active && player
    ? Math.max(0, Number(player.position || 0) / 1000) : 0
  readonly property real progress: active && player && player.duration > 0
    ? Math.min(1, player.position / player.duration) : 0

  visible: media && media.kind === "audio"
  height: visible ? Style.space(48) : 0

  DevicePixelButton {
    id: voiceMessageButton
    readonly property bool downloading: visible && root.service
      && root.service.mediaDownloading(root.message)

    anchors.left: parent.left
    anchors.verticalCenter: parent.verticalCenter
    width: Style.space(40)
    height: Style.space(40)
    iconText: downloading ? "󰔟"
      : (root.downloaded ? (root.playing ? "󰏤" : "󰐊") : "󰇚")
    tooltipText: downloading ? "Downloading voice message"
      : (root.downloaded
        ? (root.playing ? "Pause voice message" : "Play voice message")
        : "Download voice message")
    foreground: root.foreground
    accent: root.accent
    enabled: root.service && !downloading
    devicePixelRatio: root.devicePixelRatio

    onClicked: {
      if (!root.panel) return
      if (root.downloaded) root.panel.toggleVoiceMessage(root)
      else root.panel.downloadMedia(root.message, root.delegateItem)
    }
  }

  Column {
    anchors.left: voiceMessageButton.right
    anchors.right: parent.right
    anchors.leftMargin: Style.space(10)
    anchors.verticalCenter: parent.verticalCenter
    spacing: Style.space(6)

    Text {
      objectName: "voiceMessageTitle"
      width: parent.width
      text: root.media && root.media.voice_message === false
        ? "Audio" : "Voice message"
      color: root.foreground
      font.family: root.fontFamily
      font.pixelSize: Style.font.body
      font.bold: true
      elide: Text.ElideRight
    }

    Item {
      width: parent.width
      height: Math.max(voiceDuration.implicitHeight, Style.space(8))

      Rectangle {
        id: voiceProgressTrack
        anchors.left: parent.left
        anchors.right: voiceDuration.left
        anchors.rightMargin: Style.space(10)
        anchors.verticalCenter: parent.verticalCenter
        height: Math.max(2, Style.normalBorderWidth)
        radius: height / 2
        color: Style.normalBorderFor(root.foreground, root.accent)

        Rectangle {
          objectName: "voiceProgressFill"
          width: parent.width * root.progress
          height: parent.height
          radius: parent.radius
          color: root.accent
        }

        MouseArea {
          anchors.fill: parent
          enabled: root.active && root.player && root.player.duration > 0
          cursorShape: enabled ? Qt.PointingHandCursor : Qt.ArrowCursor
          onClicked: function (mouse) {
            root.player.position = Math.round(
              mouse.x / width * root.player.duration)
          }
        }
      }

      Text {
        id: voiceDuration
        objectName: "voiceMessageDuration"
        anchors.right: parent.right
        anchors.verticalCenter: parent.verticalCenter
        text: Model.mediaDuration(root.active
          ? root.elapsedSeconds : root.totalSeconds)
        color: root.timestamp
        font.family: root.fontFamily
        font.pixelSize: root.metaFontSize
      }
    }
  }

  Component.onDestruction: if (panel
    && typeof panel.stopVoiceMessage === "function")
    panel.stopVoiceMessage(root)
}
