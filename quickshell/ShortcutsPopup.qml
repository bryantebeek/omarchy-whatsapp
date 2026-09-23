import QtQuick
import QtQuick.Controls as QQC
import qs.Commons

QQC.Popup {
  id: root

  property real devicePixelRatio: 1

  readonly property color accent: Color.accent
  readonly property string fontFamily: Style.font.family
  readonly property color mutedText: Qt.rgba(Color.popups.text.r,
    Color.popups.text.g, Color.popups.text.b, Color.popups.text.a * 0.72)
  readonly property var popupBorderSpec:
    Border.localOrSurfaceSpec("popups", "border",
      Color.popups.border, Color.popups.border,
      Math.max(1, Style.normalBorderWidth))
  readonly property var sections: [
    { title: "Conversations", shortcuts: [
      { keys: "Ctrl+1 … Ctrl+0", action: "Open a visible conversation" },
      { keys: "Hold Ctrl", action: "Show conversation numbers" },
      { keys: "Ctrl+[  Ctrl+]", action: "Previous or next conversation" },
      { keys: "↑  ↓", action: "Move through the sidebar" },
      { keys: "→", action: "Focus the message composer" },
      { keys: "←", action: "Back to the sidebar from the start of the composer" }
    ] },
    { title: "Search", shortcuts: [
      { keys: "Ctrl+F  Ctrl+K  Ctrl+L", action: "Search, or clear the search" },
      { keys: "Ctrl+U", action: "Show only unread conversations" },
      { keys: "↓  Enter", action: "Open the first result" },
      { keys: "Esc", action: "Leave search" }
    ] },
    { title: "Messages", shortcuts: [
      { keys: "Enter", action: "Send the message" },
      { keys: "Ctrl+V", action: "Paste an image" },
      { keys: "Ctrl+↓", action: "Jump to the latest message" },
      { keys: "Tab", action: "Insert the highlighted mention" }
    ] },
    { title: "General", shortcuts: [
      { keys: "Ctrl+?", action: "Show keyboard shortcuts" }
    ] }
  ]

  component CrispBorderSurface: DevicePixelBorderSurface {
    devicePixelRatio: root.devicePixelRatio
  }

  component CrispButton: DevicePixelButton {
    devicePixelRatio: root.devicePixelRatio
  }

  x: Math.round((parent.width - width) / 2)
  y: Math.round((parent.height - height) / 2)
  width: Math.min(Style.space(560), parent.width - Style.space(48))
  height: Math.min(implicitHeight, parent.height - Style.space(48))
  padding: 0
  leftPadding: Border.left(popupBorderSpec)
  rightPadding: Border.right(popupBorderSpec)
  topPadding: Border.top(popupBorderSpec)
  bottomPadding: Border.bottom(popupBorderSpec)
  modal: true
  focus: true
  closePolicy: QQC.Popup.CloseOnEscape | QQC.Popup.CloseOnPressOutside

  // Opened from a key handler, so the popup does not take focus by itself
  // and CloseOnEscape never sees the key.
  onOpened: Qt.callLater(function() { root.contentItem.forceActiveFocus() })

  background: CrispBorderSurface {
    color: Color.popups.background
    sourceBorderSpec: root.popupBorderSpec
    radius: Style.cornerRadius + Style.space(4)
  }

  contentItem: Column {
    spacing: 0
    Keys.onEscapePressed: root.close()

    Item {
      id: shortcutsHeader

      width: parent.width
      height: Style.space(58)

      Text {
        anchors.left: parent.left
        anchors.leftMargin: Style.space(18)
        anchors.verticalCenter: parent.verticalCenter
        text: "Keyboard shortcuts"
        color: Color.popups.text
        font.family: root.fontFamily
        font.pixelSize: Style.font.title
        font.bold: true
      }

      CrispButton {
        anchors.right: parent.right
        anchors.rightMargin: Style.space(10)
        anchors.verticalCenter: parent.verticalCenter
        iconText: "󰅖"
        foreground: Color.popups.text
        accent: root.accent
        tooltipText: "Close shortcuts"
        onClicked: root.close()
      }
    }

    Rectangle {
      width: parent.width
      height: Math.max(1, Style.normalBorderWidth)
      color: Color.popups.border
    }

    Column {
      width: parent.width
      padding: Style.space(18)
      spacing: Style.space(14)

      Repeater {
        model: root.sections

        Column {
          required property var modelData

          width: parent.width - parent.padding * 2
          spacing: Style.space(4)

          Text {
            text: modelData.title
            color: root.accent
            font.family: root.fontFamily
            font.pixelSize: Style.font.caption
            font.bold: true
          }

          Repeater {
            model: modelData.shortcuts

            Item {
              required property var modelData

              width: parent.width
              height: Math.max(keysLabel.implicitHeight,
                actionLabel.implicitHeight) + Style.space(4)

              Text {
                id: actionLabel

                anchors.left: parent.left
                anchors.right: keysLabel.left
                anchors.rightMargin: Style.space(12)
                anchors.verticalCenter: parent.verticalCenter
                text: modelData.action
                color: Color.popups.text
                font.family: root.fontFamily
                font.pixelSize: Style.font.body
                elide: Text.ElideRight
              }

              Text {
                id: keysLabel

                anchors.right: parent.right
                anchors.verticalCenter: parent.verticalCenter
                text: modelData.keys
                color: root.mutedText
                font.family: root.fontFamily
                font.pixelSize: Style.font.body
                font.bold: true
              }
            }
          }
        }
      }
    }
  }
}
