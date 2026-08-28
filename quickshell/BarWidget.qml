import QtQuick
import QtQuick.Effects
import Quickshell
import qs.Commons
import qs.Ui

import "Model.js" as Model

BarWidget {
  id: root
  moduleName: "io.github.bryantebeek.whatsapp"

  readonly property var whatsapp: bar && bar.shell
    ? bar.shell.serviceFor(moduleName) : null
  readonly property int unread: whatsapp ? whatsapp.unreadTotal : 0
  readonly property string connectionState: whatsapp
    ? whatsapp.connectionState : "starting"
  readonly property bool showCount: root.setting("showUnreadCount", true) === true
  readonly property bool hideWhenEmpty: root.setting("hideWhenEmpty", false) === true
  readonly property url whatsappIcon: Qt.resolvedUrl("icons/brand-whatsapp-filled.svg")
  readonly property string unreadLabel: unread > 99 ? "99+" : String(unread)

  visible: !hideWhenEmpty || unread > 0
  implicitWidth: button.implicitWidth
  implicitHeight: button.implicitHeight

  function openPanel(chatJid) {
    if (whatsapp) whatsapp.openPanel(chatJid || "")
  }

  TextMetrics {
    id: unreadLabelMetrics
    text: root.unreadLabel
    font.family: Style.font.family
    font.pixelSize: Style.font.caption
    font.bold: true
  }

  Component {
    id: whatsappBarIcon

    Item {
      id: whatsappBarContent

      Row {
        anchors.centerIn: parent
        height: parent.height
        spacing: Style.space(4)

        Text {
          visible: root.showCount && root.unread > 0
          anchors.verticalCenter: parent.verticalCenter
          text: root.unreadLabel
          color: Color.accent
          font.family: Style.font.family
          font.pixelSize: Style.font.caption
          font.bold: true
          renderType: Text.NativeRendering
        }

        Image {
          anchors.verticalCenter: parent.verticalCenter
          width: Math.max(1, whatsappBarContent.width - Style.space(1))
          height: width
          source: root.whatsappIcon
          sourceSize: Qt.size(width, height)
          fillMode: Image.PreserveAspectFit
          smooth: true
          layer.enabled: true
          layer.smooth: true
          layer.effect: MultiEffect {
            colorization: 1
            colorizationColor: Color.accent
          }
        }
      }
    }
  }

  BarIconButton {
    id: button
    anchors.fill: parent
    bar: root.bar
    iconComponent: whatsappBarIcon
    slotSize: Style.bar.statusSlot
      + (root.showCount && root.unread > 0
        ? Math.ceil(unreadLabelMetrics.advanceWidth) + Style.space(4) : 0)
    active: root.unread > 0
    useActiveColor: false
    dimmed: root.connectionState !== "connected"
    tooltipText: root.connectionState === "connected"
      ? (root.unread > 0 ? "WhatsApp · " + root.unread + " unread" : "WhatsApp")
      : "WhatsApp · " + Model.connectionLabel(root.connectionState)
    onPressed: function(buttonCode) {
      if (buttonCode === Qt.LeftButton) root.openPanel("")
      else if (buttonCode === Qt.RightButton && root.whatsapp) root.whatsapp.refresh()
    }
  }
}
