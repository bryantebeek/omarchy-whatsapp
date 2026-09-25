import QtQuick
import QtQuick.Controls as QQC
import qs.Commons

// Six quick reactions plus a hand-off to Omarchy's emoji picker. The picker
// runs in another shell surface and can only give an emoji back by pasting it,
// so a hidden field waits for that text while the request is outstanding.
QQC.Popup {
  id: root

  property var shell: null
  property string ownReactionEmoji: ""
  property real devicePixelRatio: 1
  property string fontFamily: Style.font.family
  property real metaFontSize: Style.font.caption
  property color foreground: Color.foreground
  property color muted: Color.muted
  property color accent: Color.accent
  property bool waitingForEmojiPicker: false
  readonly property alias pasteTarget: emojiPasteTarget
  readonly property var popupBorderSpec:
    Border.localOrSurfaceSpec("popups", "border",
      Color.popups.border, Color.popups.border,
      Math.max(1, Style.normalBorderWidth))

  property bool replyEnabled: false
  signal replyChosen()
  signal reactionChosen(string emoji)

  function openOmarchyEmojiPicker() {
    waitingForEmojiPicker = true
    emojiPasteTarget.text = ""
    emojiPasteTarget.forceActiveFocus()
    Qt.callLater(function () {
      if (!root.shell || typeof root.shell.summon !== "function"
          || !root.shell.summon("omarchy.emojis", "{}"))
        root.waitingForEmojiPicker = false
    })
  }

  function acceptEmojiPickerText() {
    var value = emojiPasteTarget.text.trim()
    if (!waitingForEmojiPicker || !value) return
    waitingForEmojiPicker = false
    Qt.callLater(function () {
      root.reactionChosen(value)
    })
  }

  width: Style.space(328)
  height: reactionPickerColumn.implicitHeight + topPadding + bottomPadding
  margins: Style.space(8)
  padding: Style.space(10)
  leftPadding: padding + Border.left(popupBorderSpec)
  rightPadding: padding + Border.right(popupBorderSpec)
  topPadding: padding + Border.top(popupBorderSpec)
  bottomPadding: padding + Border.bottom(popupBorderSpec)
  modal: false
  focus: true
  closePolicy: QQC.Popup.CloseOnEscape | QQC.Popup.CloseOnPressOutsideParent

  onOpened: {
    waitingForEmojiPicker = false
    emojiPasteTarget.text = ""
  }
  onClosed: waitingForEmojiPicker = false

  background: DevicePixelBorderSurface {
    color: Color.popups.background
    sourceBorderSpec: root.popupBorderSpec
    devicePixelRatio: root.devicePixelRatio
    radius: Style.cornerRadius + Style.space(4)
  }

  contentItem: Column {
    id: reactionPickerColumn
    spacing: Style.space(8)

    DevicePixelButton {
      objectName: "replyToMessageButton"
      visible: root.replyEnabled
      width: parent.width
      text: "Reply"
      iconText: ""
      foreground: Color.popups.text
      accent: root.accent
      devicePixelRatio: root.devicePixelRatio
      onClicked: { root.close(); root.replyChosen() }
    }

    Item {
      width: parent.width
      height: Math.max(reactionTitle.implicitHeight, reactionHint.implicitHeight)

      Text {
        id: reactionTitle
        anchors.left: parent.left
        anchors.verticalCenter: parent.verticalCenter
        text: "React to message"
        color: Color.popups.text
        font.family: root.fontFamily
        font.pixelSize: Style.font.body
        font.bold: true
      }
      Text {
        id: reactionHint
        objectName: "reactionPickerHint"
        anchors.right: parent.right
        anchors.verticalCenter: parent.verticalCenter
        text: root.ownReactionEmoji !== ""
          ? "Tap selected to remove" : "Choose one"
        color: root.muted
        font.family: root.fontFamily
        font.pixelSize: root.metaFontSize
      }
    }

    Row {
      id: quickReactionRow
      width: parent.width
      spacing: Style.space(6)

      Repeater {
        model: ["👍", "❤️", "😂", "😮", "😢", "🙏"]

        delegate: DevicePixelBorderSurface {
          id: quickReaction
          required property string modelData
          readonly property bool selected: root.ownReactionEmoji === modelData
          readonly property bool hot: quickReactionHover.hovered

          width: (quickReactionRow.width - quickReactionRow.spacing * 5) / 6
          height: width
          radius: width / 2
          devicePixelRatio: root.devicePixelRatio
          color: selected
            ? Style.selectedFillFor(root.foreground, root.accent)
            : hot
              ? Style.hoverFillFor(root.foreground, root.accent)
              : "transparent"
          sourceBorderSpec: selected
            ? Border.controlSpec("selected", root.foreground, root.accent)
            : hot
              ? Border.controlSpec("hover-cursor", root.foreground, root.accent)
              : Border.none()

          Behavior on color {
            ColorAnimation {
              duration: 100
            }
          }

          Text {
            anchors.centerIn: parent
            text: quickReaction.modelData
            font.pixelSize: Style.font.display
            scale: quickReaction.hot ? 1.08 : 1

            Behavior on scale {
              NumberAnimation {
                duration: 100
                easing.type: Easing.OutCubic
              }
            }
          }
          HoverHandler {
            id: quickReactionHover
          }
          MouseArea {
            anchors.fill: parent
            cursorShape: Qt.PointingHandCursor
            onClicked: root.reactionChosen(quickReaction.modelData)
          }
        }
      }
    }

    Rectangle {
      width: parent.width
      height: Math.max(1, Style.normalBorderWidth)
      color: Style.normalBorderFor(root.foreground, root.accent)
    }

    Item {
      width: parent.width
      height: Style.space(36)

      DevicePixelTextField {
        id: emojiPasteTarget
        anchors.left: parent.left
        anchors.bottom: parent.bottom
        width: 1
        height: 1
        opacity: 0
        activeFocusOnTab: false
        devicePixelRatio: root.devicePixelRatio
        onTextChanged: root.acceptEmojiPickerText()
      }
      DevicePixelButton {
        objectName: "reactionPickerEmojiButton"
        anchors.fill: parent
        iconText: ""
        text: root.waitingForEmojiPicker
          ? "Choose an emoji…" : "Choose any emoji"
        tooltipText: "Open the Omarchy emoji picker"
        foreground: Color.popups.text
        accent: root.accent
        bordered: true
        devicePixelRatio: root.devicePixelRatio
        onClicked: root.openOmarchyEmojiPicker()
      }
    }
  }
}
