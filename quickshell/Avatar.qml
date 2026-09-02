import QtQuick
import QtQuick.Effects
import qs.Commons

import "Model.js" as Model

// Circular profile preview with an initials fallback. The daemon delivers
// avatars asynchronously and may have none at all, so the initials stay
// visible until an image has actually rendered and come back the moment a
// source is cleared or fails.
DevicePixelBorderSurface {
  id: root

  property var service: null
  property string jid: ""
  property string name: ""
  property real size: Style.space(34)
  property real initialsPixelSize: Style.font.caption
  property string fontFamily: Style.font.family
  property color foreground: Color.foreground
  property color accent: Color.accent
  property string imageObjectName: ""
  // Poll voters are not part of any chat snapshot, so their previews have to
  // be requested by the card that renders them.
  property bool requestOnLoad: false
  readonly property alias hasRenderedAvatar: avatarImage.hasRenderedAvatar
  readonly property alias initials: initialsLabel.text

  width: size
  height: size
  radius: width / 2
  clip: true
  color: Style.normalFillFor(foreground, accent)
  sourceBorderSpec: Border.flat(Style.normalBorderFor(foreground, accent),
    Math.max(1, Style.normalBorderWidth))

  Component.onCompleted: if (requestOnLoad && service) service.requestAvatar(jid)

  Text {
    id: initialsLabel
    anchors.centerIn: parent
    visible: !avatarImage.hasRenderedAvatar
    // Without an identity there is nothing to abbreviate: the conversation
    // header renders before a chat is selected.
    text: root.jid || root.name
      ? Model.initials(root.name, root.jid) : "?"
    color: root.foreground
    font.family: root.fontFamily
    font.pixelSize: root.initialsPixelSize
    font.bold: true
  }

  Rectangle {
    id: avatarMask
    anchors.fill: parent
    radius: width / 2
    visible: false
    layer.enabled: true
  }

  Image {
    id: avatarImage
    objectName: root.imageObjectName
    property bool hasRenderedAvatar: false
    anchors.fill: parent
    source: root.service ? root.service.avatarUrl(root.jid) : ""
    asynchronous: true
    cache: false
    retainWhileLoading: true
    fillMode: Image.PreserveAspectCrop
    onSourceChanged: if (String(source) === "") hasRenderedAvatar = false
    onStatusChanged: {
      if (status === Image.Ready) hasRenderedAvatar = true
      else if (status === Image.Error) hasRenderedAvatar = false
    }
    layer.enabled: true
    layer.smooth: true
    layer.effect: MultiEffect {
      maskEnabled: true
      maskSource: avatarMask
      maskThresholdMin: 0.5
      maskSpreadAtMin: 1.0
    }
  }
}
