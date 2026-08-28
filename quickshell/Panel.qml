import QtQuick
import QtQuick.Controls as QQC
import QtQuick.Effects
import Quickshell
import Quickshell.Io
import qs.Commons
import qs.Ui

import "Model.js" as Model

Item {
  id: root

  property var shell: null
  property var manifest: null
  property var service: null
  property bool opened: false
  property bool controlHeld: false
  property int controlActivatorKey: 0
  property bool newChatVisible: false
  property bool conversationReady: false
  property int handledMessagesNavigationSerial: 0
  property int conversationScrollSerial: 0
  property bool scrollToBottomAfterMessages: false
  property var licenseEntries: []
  property string licenseLoadError: ""
  readonly property bool unreadOnly: service && service.unreadOnly === true

  readonly property string pluginId: manifest && manifest.id
    ? String(manifest.id) : "io.github.bryantebeek.whatsapp"
  readonly property color foreground: Color.foreground
  readonly property color background: Color.background
  readonly property color accent: Color.accent
  readonly property color muted: Color.muted
  readonly property color timestamp: Qt.rgba(foreground.r, foreground.g,
    foreground.b, foreground.a * 0.72)
  readonly property color sidebarSecondary: Qt.rgba(foreground.r, foreground.g,
    foreground.b, foreground.a * 0.82)
  readonly property string fontFamily: Style.font.family
  readonly property int messageMetaFontSize: Math.max(8, Style.font.caption - 1)
  readonly property url whatsappIcon: Qt.resolvedUrl("icons/brand-whatsapp-filled.svg")
  readonly property string licenseReportPath: {
    var value = String(Qt.resolvedUrl("licenses.json"))
    return value.indexOf("file://") === 0
      ? decodeURIComponent(value.substring(7)) : value
  }
  readonly property bool paired: service && service.connectionState === "connected"
  readonly property bool pairing: service && service.connectionState === "pairing"
  readonly property real devicePixelRatio: window.screen
    ? window.screen.devicePixelRatio : 1
  readonly property var filteredChats: {
    var source = service && Array.isArray(service.chats) ? service.chats : []
    var selectedJid = service ? String(service.selectedChatJid || "") : ""
    var query = String(chatSearch.text || "").trim().toLowerCase()
    if (!query && !unreadOnly) return source
    var output = []
    for (var i = 0; i < source.length; i++) {
      var chat = source[i] || {}
      var isSelected = String(chat.jid || "") === selectedJid
      if (unreadOnly && Number(chat.unread || 0) <= 0 && !isSelected) continue
      var haystack = (Model.friendlyName(chat.name, chat.jid) + "\n"
        + String(chat.last_message || "") + "\n" + String(chat.jid || "")).toLowerCase()
      if (!query || haystack.indexOf(query) >= 0) output.push(chat)
    }
    return output
  }

  function snapToDevicePixel(value) {
    var scale = Math.max(1, Number(devicePixelRatio) || 1)
    return Math.round(Number(value) * scale) / scale
  }

  function devicePixelBorderSpec(spec) {
    var scale = Math.max(1, Number(devicePixelRatio) || 1)
    function snappedWidth(value) {
      var width = Math.max(0, Number(value) || 0)
      return width > 0 ? Math.max(1, Math.round(width * scale)) / scale : 0
    }
    var borderColor = Border.color(spec)
    var gradient = spec && spec.gradient && spec.gradient.enabled
      ? spec.gradient
      : { colors: [borderColor, borderColor], angle: 0, enabled: true }
    return {
      color: borderColor,
      widths: {
        top: snappedWidth(Border.top(spec)),
        right: snappedWidth(Border.right(spec)),
        bottom: snappedWidth(Border.bottom(spec)),
        left: snappedWidth(Border.left(spec))
      },
      gradient: gradient
    }
  }

  component CrispBorderSurface: BorderSurface {
    property var sourceBorderSpec: Border.none()
    borderSpec: root.devicePixelBorderSpec(sourceBorderSpec)
  }

  component CrispButton: Button {
    borderSpec: root.devicePixelBorderSpec(_borderSpec)
  }

  component CrispTextField: TextField {
    id: crispTextField
    background: CrispBorderSurface {
      color: Style.controlFill(crispTextField._focused,
        crispTextField._hot, crispTextField.foreground, crispTextField.accent)
      sourceBorderSpec: crispTextField._borderSpec
      radius: Style.cornerRadius
    }
  }

  FileView {
    id: licenseReportFile

    path: root.licenseReportPath
    printErrors: false
    watchChanges: true
    onLoaded: {
      try {
        var report = JSON.parse(text())
        root.licenseEntries = report && Array.isArray(report.entries)
          ? report.entries : []
        root.licenseLoadError = ""
      } catch (error) {
        root.licenseEntries = []
        root.licenseLoadError = "The bundled license report could not be read."
      }
    }
    onLoadFailed: {
      root.licenseEntries = []
      root.licenseLoadError = "The bundled license report is unavailable."
    }
  }

  function filteredLicenses(query) {
    var needle = String(query || "").trim().toLowerCase()
    if (!needle) return licenseEntries
    var matches = []
    for (var i = 0; i < licenseEntries.length; i++) {
      var entry = licenseEntries[i] || {}
      var haystack = (String(entry.name || "") + "\n"
        + String(entry.version || "") + "\n"
        + String(entry.license || "")).toLowerCase()
      if (haystack.indexOf(needle) >= 0) matches.push(entry)
    }
    return matches
  }

  function licenseKindLabel(kind) {
    if (kind === "project") return "Application"
    if (kind === "asset") return "Bundled asset"
    return "Rust package"
  }

  function open(payloadJson) {
    var payload = {}
    try { payload = JSON.parse(payloadJson || "{}") || {} } catch (error) {}
    opened = true
    if (service) {
      service.refreshMetadata()
      var targetJid = payload.chatJid
        ? String(payload.chatJid) : String(service.selectedChatJid || "")
      if (targetJid) service.selectChat(targetJid)
      service.setPanelState(true, true)
    }
    Qt.callLater(function() {
      root.revealSelectedChat()
      root.prepareCurrentConversation()
      focusScope.forceActiveFocus()
      if (service && service.selectedChatJid) composer.forceActiveFocus()
    })
  }

  function close() {
    opened = false
    controlHeld = false
    controlActivatorKey = 0
    if (service) service.setPanelState(false, false)
  }

  function requestClose() {
    if (shell && typeof shell.hide === "function") shell.hide(pluginId)
    else close()
  }

  function chooseChat(jid) {
    if (!service) return
    service.selectChat(String(jid || ""))
    Qt.callLater(function() {
      root.revealSelectedChat()
      composer.forceActiveFocus()
    })
  }

  function firstVisibleChatIndex() {
    if (!chatList || !chatList.count) return -1
    var index = chatList.indexAt(1, chatList.contentY + 1)
    return index >= 0 ? index : 0
  }

  function chatShortcutSlot(key) {
    if (key >= Qt.Key_1 && key <= Qt.Key_9) return key - Qt.Key_1
    if (key === Qt.Key_0) return 9
    return -1
  }

  function isControlActivator(event) {
    if (!event) return false
    if (event.key === Qt.Key_Control) return true
    if (!(event.modifiers & Qt.ControlModifier)) return false
    return event.key === Qt.Key_CapsLock
      || event.key === Qt.Key_Shift
      || event.key === Qt.Key_Alt
      || event.key === Qt.Key_AltGr
      || event.key === Qt.Key_Meta
  }

  function beginControlHold(event) {
    if (controlHeld || event.isAutoRepeat || !isControlActivator(event)) return false
    controlActivatorKey = event.key
    controlHeld = true
    return true
  }

  function endControlHold(event) {
    if (!controlHeld || event.isAutoRepeat
        || event.key !== controlActivatorKey) return false
    controlHeld = false
    controlActivatorKey = 0
    return true
  }

  function chatShortcutLabelForIndex(index) {
    var slot = index - firstVisibleChatIndex()
    if (slot < 0 || slot > 9) return ""
    return "^" + (slot === 9 ? "0" : String(slot + 1))
  }

  function chooseChatShortcut(slot) {
    var firstIndex = firstVisibleChatIndex()
    var index = firstIndex + slot
    if (firstIndex < 0 || index < 0 || index >= filteredChats.length) return false
    chooseChat(filteredChats[index].jid)
    return true
  }

  function revealSelectedChat() {
    if (!service || !service.selectedChatJid || !chatList) return
    for (var i = 0; i < filteredChats.length; i++) {
      if (String(filteredChats[i].jid || "") === service.selectedChatJid) {
        chatList.currentIndex = i
        chatList.positionViewAtIndex(i, ListView.Contain)
        return
      }
    }
    chatList.currentIndex = -1
  }

  function openNewChat() {
    var jid = Model.normalizedJid(newChat.text)
    if (!jid || !service) return
    newChat.text = ""
    newChatVisible = false
    service.selectChat(jid)
    Qt.callLater(function() { composer.forceActiveFocus() })
  }

  function submitMessage() {
    if (!service || !service.sendMessage(composer.text)) return
    composer.text = ""
    scheduleConversationScroll("bottom", "")
  }

  function messageIndex(messageId) {
    if (!service || !messageId) return -1
    for (var i = 0; i < service.messages.length; i++)
      if (String(service.messages[i].id || "") === String(messageId)) return i
    return -1
  }

  function positionConversationScroll(mode, messageId, serial) {
    if (serial !== conversationScrollSerial
        || !messageList) return
    if (messageList.count) {
      messageList.forceLayout()
      if (mode === "unread") {
        var index = messageIndex(messageId)
        if (index >= 0) messageList.positionViewAtIndex(index, ListView.Beginning)
        else messageList.positionViewAtBeginning()
      } else {
        messageList.positionViewAtEnd()
      }
    }
    conversationReady = true
  }

  function scheduleConversationScroll(mode, messageId) {
    var scrollMode = String(mode || "bottom")
    var scrollMessageId = String(messageId || "")
    var serial = ++conversationScrollSerial
    Qt.callLater(function() {
      root.positionConversationScroll(scrollMode, scrollMessageId, serial)
    })
  }

  function prepareCurrentConversation() {
    if (conversationReady || !service
        || service.messagesChatJid !== service.selectedChatJid) return
    handledMessagesNavigationSerial = Number(
      service.messagesNavigationSerial || 0)
    var firstUnreadId = String(service.messagesFirstUnreadId || "")
    scheduleConversationScroll(firstUnreadId ? "unread" : "bottom",
      firstUnreadId)
  }

  Connections {
    target: root.service
    function onSelectedChatJidChanged() {
      root.conversationScrollSerial++
      root.conversationReady = false
      root.scrollToBottomAfterMessages = false
      Qt.callLater(root.revealSelectedChat)
    }
    function onMessageSentSerialChanged() {
      root.scrollToBottomAfterMessages = true
    }
    function onMessagesResponseSerialChanged() {
      if (!root.service
          || root.service.messagesChatJid !== root.service.selectedChatJid) return
      if (root.service.messagesResponseHasFollowup === true) return
      var serial = Number(root.service.messagesNavigationSerial || 0)
      if (serial !== root.handledMessagesNavigationSerial) {
        root.handledMessagesNavigationSerial = serial
        root.scrollToBottomAfterMessages = false
        var firstUnreadId = String(root.service.messagesFirstUnreadId || "")
        root.scheduleConversationScroll(firstUnreadId ? "unread" : "bottom",
          firstUnreadId)
      } else if (root.scrollToBottomAfterMessages) {
        root.scrollToBottomAfterMessages = false
        root.scheduleConversationScroll("bottom", "")
      } else if (!root.conversationReady) {
        root.prepareCurrentConversation()
      }
    }
  }

  FloatingWindow {
    id: window
    visible: root.opened
    title: "WhatsApp"
    color: root.background
    implicitWidth: 1080
    implicitHeight: 720
    minimumSize: Qt.size(780, 520)

    onVisibleChanged: {
      if (!visible && root.opened) root.requestClose()
    }

    FocusScope {
      id: focusScope
      anchors.fill: parent
      focus: true
      Keys.priority: Keys.BeforeItem

      onActiveFocusChanged: if (root.service)
        root.service.setPanelState(root.opened, activeFocus)

      Keys.onShortcutOverride: function(event) {
        if (root.beginControlHold(event)) event.accepted = true
      }

      Keys.onPressed: function(event) {
        if (root.beginControlHold(event)
            || event.key === root.controlActivatorKey) {
          event.accepted = true
          return
        }
        if (!(event.modifiers & Qt.ControlModifier)) return
        var shortcutSlot = root.chatShortcutSlot(event.key)
        if (shortcutSlot >= 0 && root.chooseChatShortcut(shortcutSlot)) {
          event.accepted = true
        } else if (event.key === Qt.Key_K || event.key === Qt.Key_F) {
          chatSearch.forceActiveFocus()
          event.accepted = true
        }
      }

      Keys.onReleased: function(event) {
        if (root.endControlHold(event)) event.accepted = true
      }

      QQC.Popup {
        id: licensesPopup

        readonly property var popupBorderSpec:
          Border.localOrSurfaceSpec("popups", "border",
            Color.popups.border, Color.popups.border,
            Math.max(1, Style.normalBorderWidth))

        parent: focusScope
        x: Math.round((parent.width - width) / 2)
        y: Math.round((parent.height - height) / 2)
        width: Math.min(Style.space(760), parent.width - Style.space(48))
        height: Math.min(Style.space(620), parent.height - Style.space(48))
        padding: 0
        leftPadding: Border.left(popupBorderSpec)
        rightPadding: Border.right(popupBorderSpec)
        topPadding: Border.top(popupBorderSpec)
        bottomPadding: Border.bottom(popupBorderSpec)
        modal: true
        focus: true
        closePolicy: QQC.Popup.CloseOnEscape
          | QQC.Popup.CloseOnPressOutside

        onOpened: {
          licenseSearch.text = ""
          Qt.callLater(function() { licenseSearch.forceActiveFocus() })
        }

        background: CrispBorderSurface {
          color: Color.popups.background
          sourceBorderSpec: licensesPopup.popupBorderSpec
          radius: Style.cornerRadius + Style.space(4)
        }

        contentItem: Column {
          spacing: 0

          Item {
            id: licensesHeader

            width: parent.width
            height: Style.space(68)

            Column {
              anchors.left: parent.left
              anchors.leftMargin: Style.space(18)
              anchors.verticalCenter: parent.verticalCenter
              spacing: Style.space(2)

              Text {
                text: "Open-source licenses"
                color: Color.popups.text
                font.family: root.fontFamily
                font.pixelSize: Style.font.title
                font.bold: true
              }
              Text {
                text: root.licenseEntries.length + " licensed components"
                color: Qt.rgba(Color.popups.text.r, Color.popups.text.g,
                  Color.popups.text.b, Color.popups.text.a * 0.72)
                font.family: root.fontFamily
                font.pixelSize: Style.font.caption
              }
            }

            CrispButton {
              anchors.right: parent.right
              anchors.rightMargin: Style.space(10)
              anchors.verticalCenter: parent.verticalCenter
              iconText: "󰅖"
              foreground: Color.popups.text
              accent: root.accent
              tooltipText: "Close licenses"
              focusable: true
              onClicked: licensesPopup.close()
            }
          }

          Rectangle {
            width: parent.width
            height: Math.max(1, Style.normalBorderWidth)
            color: Color.popups.border
          }

          Item {
            id: licenseSearchRow

            width: parent.width
            height: Style.space(58)

            CrispTextField {
              id: licenseSearch

              anchors.fill: parent
              anchors.margins: Style.space(10)
              placeholderText: "Search packages or licenses"
              foreground: Color.popups.text
              accent: root.accent
            }
          }

          Rectangle {
            width: parent.width
            height: Math.max(1, Style.normalBorderWidth)
            color: Color.popups.border
          }

          Item {
            width: parent.width
            height: parent.height - licensesHeader.height
              - licenseSearchRow.height - Style.normalBorderWidth * 2

            ListView {
              id: licenseList

              anchors.fill: parent
              anchors.margins: Style.space(6)
              clip: true
              spacing: Style.space(2)
              model: root.filteredLicenses(licenseSearch.text)

              delegate: Rectangle {
                required property var modelData
                required property int index

                width: licenseList.width
                height: Style.space(56)
                radius: Style.cornerRadius
                color: index % 2 === 0
                  ? Style.hoverFillFor(Color.popups.text, root.accent)
                  : "transparent"

                Column {
                  anchors.left: parent.left
                  anchors.leftMargin: Style.space(10)
                  anchors.right: licenseValue.left
                  anchors.rightMargin: Style.space(16)
                  anchors.verticalCenter: parent.verticalCenter
                  spacing: Style.space(2)

                  Text {
                    width: parent.width
                    text: String(modelData.name || "")
                      + (modelData.version ? " " + modelData.version : "")
                    color: Color.popups.text
                    font.family: root.fontFamily
                    font.pixelSize: Style.font.body
                    font.bold: modelData.kind !== "rust"
                    elide: Text.ElideRight
                  }
                  Text {
                    width: parent.width
                    text: root.licenseKindLabel(String(modelData.kind || ""))
                    color: Qt.rgba(Color.popups.text.r, Color.popups.text.g,
                      Color.popups.text.b, Color.popups.text.a * 0.68)
                    font.family: root.fontFamily
                    font.pixelSize: Style.font.caption
                    elide: Text.ElideRight
                  }
                }

                Text {
                  id: licenseValue

                  anchors.right: parent.right
                  anchors.rightMargin: Style.space(10)
                  anchors.verticalCenter: parent.verticalCenter
                  width: Math.min(parent.width * 0.46, implicitWidth)
                  text: String(modelData.license || "Not declared")
                  color: root.accent
                  font.family: root.fontFamily
                  font.pixelSize: Style.font.caption
                  horizontalAlignment: Text.AlignRight
                  elide: Text.ElideRight
                }
              }
            }

            Text {
              anchors.centerIn: parent
              visible: licenseList.count === 0
              text: root.licenseLoadError !== ""
                ? root.licenseLoadError : "No matching licenses"
              color: Qt.rgba(Color.popups.text.r, Color.popups.text.g,
                Color.popups.text.b, Color.popups.text.a * 0.72)
              font.family: root.fontFamily
              font.pixelSize: Style.font.body
            }
          }
        }
      }

      Rectangle { anchors.fill: parent; color: root.background }

      Column {
        anchors.fill: parent
        spacing: 0

        Item {
          id: header
          width: parent.width
          height: root.snapToDevicePixel(Style.space(58))

          Row {
            anchors.left: parent.left
            anchors.leftMargin: Style.space(18)
            anchors.verticalCenter: parent.verticalCenter
            spacing: Style.space(10)

            Image {
              width: Style.font.iconLarge
              height: width
              source: root.whatsappIcon
              sourceSize: Qt.size(width, height)
              fillMode: Image.PreserveAspectFit
              smooth: true
              anchors.verticalCenter: parent.verticalCenter
              layer.enabled: true
              layer.smooth: true
              layer.effect: MultiEffect {
                colorization: 1
                colorizationColor: root.accent
              }
            }
            Column {
              anchors.verticalCenter: parent.verticalCenter
              spacing: Style.space(1)
              Text {
                text: "WhatsApp"
                color: root.foreground
                font.family: root.fontFamily
                font.pixelSize: Style.font.title
                font.bold: true
              }
              Text {
                text: root.service ? Model.connectionLabel(root.service.connectionState) : "Starting"
                color: root.paired ? root.accent : root.muted
                font.family: root.fontFamily
                font.pixelSize: Style.font.caption
              }
            }
          }

          Row {
            anchors.right: parent.right
            anchors.rightMargin: Style.space(12)
            anchors.verticalCenter: parent.verticalCenter
            spacing: Style.space(4)
            CrispButton {
              id: headerMoreButton

              iconText: "󰇙"
              foreground: root.foreground
              tooltipText: "More"
              focusable: true
              selected: headerMenu.opened
              onClicked: headerMenu.opened ? headerMenu.close() : headerMenu.open()

              QQC.Popup {
                id: headerMenu

                readonly property var popupBorderSpec:
                  Border.localOrSurfaceSpec("popups", "border",
                    Color.popups.border, Color.popups.border,
                    Math.max(1, Style.normalBorderWidth))

                parent: headerMoreButton
                x: headerMoreButton.width - width
                y: headerMoreButton.height + Style.space(4)
                width: Style.space(180)
                height: headerMenuColumn.implicitHeight
                  + topPadding + bottomPadding
                margins: Style.space(8)
                padding: Style.space(6)
                leftPadding: padding + Border.left(popupBorderSpec)
                rightPadding: padding + Border.right(popupBorderSpec)
                topPadding: padding + Border.top(popupBorderSpec)
                bottomPadding: padding + Border.bottom(popupBorderSpec)
                modal: false
                focus: true
                closePolicy: QQC.Popup.CloseOnEscape
                  | QQC.Popup.CloseOnPressOutsideParent

                onOpened: Qt.callLater(function() {
                  headerLicenseAction.forceActiveFocus()
                })

                background: CrispBorderSurface {
                  color: Color.popups.background
                  sourceBorderSpec: headerMenu.popupBorderSpec
                  radius: Style.cornerRadius + Style.space(2)
                }

                contentItem: Column {
                  id: headerMenuColumn

                  spacing: Style.space(2)

                  CrispButton {
                    id: headerLicenseAction

                    width: parent.width
                    iconText: ""
                    text: "Licenses"
                    foreground: Color.popups.text
                    accent: root.accent
                    focusable: true
                    leftAlign: true
                    onClicked: {
                      headerMenu.close()
                      Qt.callLater(function() { licensesPopup.open() })
                    }
                  }

                  CrispButton {
                    width: parent.width
                    iconText: "󰅖"
                    text: "Close"
                    foreground: Color.popups.text
                    accent: root.accent
                    focusable: true
                    leftAlign: true
                    onClicked: {
                      headerMenu.close()
                      root.requestClose()
                    }
                  }
                }
              }
            }
          }

          Rectangle {
            anchors.left: parent.left
            anchors.right: parent.right
            anchors.bottom: parent.bottom
            height: Math.max(1, Style.normalBorderWidth)
            color: Style.normalBorderFor(root.foreground, root.accent)
          }
        }

        Item {
          width: parent.width
          height: parent.height - header.height

          Item {
            anchors.fill: parent
            visible: root.paired

            Row {
              anchors.fill: parent
              spacing: 0

              Item {
                id: sidebar
                width: root.snapToDevicePixel(
                  Math.min(440, Math.max(280, parent.width * 0.32)))
                height: parent.height

                Column {
                  anchors.fill: parent
                  spacing: 0

                  Column {
                    id: sidebarTools
                    width: parent.width
                    spacing: root.snapToDevicePixel(Style.space(7))
                    padding: root.snapToDevicePixel(Style.space(10))

                    Row {
                      width: parent.width - parent.padding * 2
                      spacing: root.snapToDevicePixel(Style.space(6))
                      CrispTextField {
                        id: chatSearch
                        width: parent.width - unreadFilterButton.width
                          - newChatButton.width - parent.spacing * 2
                        placeholderText: "Search conversations"
                        onAccepted: if (root.filteredChats.length)
                          root.chooseChat(root.filteredChats[0].jid)
                      }
                      CrispButton {
                        id: unreadFilterButton
                        width: root.snapToDevicePixel(implicitWidth)
                        height: root.snapToDevicePixel(implicitHeight)
                        iconText: "󰈲"
                        bordered: true
                        selected: root.unreadOnly
                        foreground: root.foreground
                        tooltipText: root.unreadOnly
                          ? "Show all conversations" : "Show unread conversations"
                        onClicked: {
                          if (root.service)
                            root.service.setUnreadOnly(!root.unreadOnly)
                          Qt.callLater(root.revealSelectedChat)
                        }
                      }
                      CrispButton {
                        id: newChatButton
                        width: root.snapToDevicePixel(implicitWidth)
                        height: root.snapToDevicePixel(implicitHeight)
                        iconText: root.newChatVisible ? "󰅖" : "󰐕"
                        bordered: true
                        foreground: root.foreground
                        tooltipText: root.newChatVisible ? "Cancel" : "New conversation"
                        onClicked: {
                          root.newChatVisible = !root.newChatVisible
                          if (root.newChatVisible)
                            Qt.callLater(function() { newChat.forceActiveFocus() })
                        }
                      }
                    }

                    Row {
                      visible: root.newChatVisible
                      width: parent.width - parent.padding * 2
                      height: visible ? newChat.implicitHeight : 0
                      spacing: Style.space(6)
                      CrispTextField {
                        id: newChat
                        width: parent.width - openChatButton.width - parent.spacing
                        placeholderText: "Phone number or JID"
                        onAccepted: root.openNewChat()
                      }
                      CrispButton {
                        id: openChatButton
                        text: "Open"
                        bordered: true
                        foreground: root.foreground
                        onClicked: root.openNewChat()
                      }
                    }
                  }

                  ListView {
                    id: chatList
                    width: parent.width
                    height: parent.height - sidebarTools.height
                    clip: true
                    model: root.filteredChats
                    boundsBehavior: Flickable.StopAtBounds
                    reuseItems: true
                    QQC.ScrollBar.vertical: QQC.ScrollBar {}

                    delegate: Item {
                      required property var modelData
                      required property int index
                      width: chatList.width
                      height: Style.space(60)
                      readonly property bool selected: root.service
                        && String(modelData.jid || "") === root.service.selectedChatJid

                      Rectangle {
                        anchors.fill: parent
                        anchors.leftMargin: Style.space(6)
                        anchors.rightMargin: Style.space(6)
                        anchors.topMargin: Style.space(2)
                        anchors.bottomMargin: Style.space(2)
                        radius: Style.cornerRadius + Style.space(6)
                        color: parent.selected
                          ? Style.selectedFillFor(root.foreground, root.accent)
                          : (rowMouse.containsMouse
                            ? Style.hoverFillFor(root.foreground, root.accent) : "transparent")
                      }

                      Row {
                        anchors.fill: parent
                        anchors.leftMargin: Style.space(12)
                        anchors.rightMargin: Style.space(12)
                        spacing: Style.space(8)
                        CrispBorderSurface {
                          width: Style.space(34)
                          height: width
                          radius: width / 2
                          clip: true
                          color: Style.normalFillFor(root.foreground, root.accent)
                          sourceBorderSpec: Border.flat(
                            Style.normalBorderFor(root.foreground, root.accent),
                            Math.max(1, Style.normalBorderWidth))
                          anchors.verticalCenter: parent.verticalCenter
                          Text {
                            anchors.centerIn: parent
                            visible: chatAvatar.status !== Image.Ready
                            text: Model.initials(modelData.name, modelData.jid)
                            color: root.foreground
                            font.family: root.fontFamily
                            font.pixelSize: Style.font.caption
                            font.bold: true
                          }
                          Rectangle {
                            id: chatAvatarMask
                            anchors.fill: parent
                            radius: width / 2
                            visible: false
                            layer.enabled: true
                          }
                          Image {
                            id: chatAvatar
                            anchors.fill: parent
                            source: root.service ? root.service.avatarUrl(modelData.jid) : ""
                            asynchronous: true
                            cache: false
                            fillMode: Image.PreserveAspectCrop
                            layer.enabled: true
                            layer.smooth: true
                            layer.effect: MultiEffect {
                              maskEnabled: true
                              maskSource: chatAvatarMask
                              maskThresholdMin: 0.5
                              maskSpreadAtMin: 1.0
                            }
                          }
                        }

                        Column {
                          width: parent.width - Style.space(42)
                          anchors.verticalCenter: parent.verticalCenter
                          spacing: Style.space(1)
                          Item {
                            id: chatTitleRow
                            width: parent.width
                            height: Math.max(chatNameLabel.implicitHeight,
                              timeLabel.height, Style.space(14))
                            Text {
                              id: chatNameLabel
                              anchors.left: parent.left
                              anchors.right: unreadBadge.left
                              anchors.rightMargin: unreadBadge.visible
                                ? Style.space(5) : 0
                              anchors.verticalCenter: parent.verticalCenter
                              text: Model.friendlyName(modelData.name, modelData.jid)
                              color: root.foreground
                              font.family: root.fontFamily
                              font.pixelSize: Style.font.body
                              font.bold: modelData.unread > 0
                              maximumLineCount: 1
                              wrapMode: Text.NoWrap
                              elide: Text.ElideRight
                            }
                            Rectangle {
                              id: unreadBadge
                              visible: Number(modelData.unread || 0) > 0
                              width: visible ? Math.max(Style.space(14),
                                unreadText.implicitWidth + Style.space(4)) : 0
                              height: visible ? Style.space(14) : 0
                              anchors.right: timeLabel.left
                              anchors.rightMargin: visible ? Style.space(5) : 0
                              anchors.verticalCenter: parent.verticalCenter
                              anchors.verticalCenterOffset: -root.snapToDevicePixel(1)
                              radius: height / 2
                              color: root.accent
                              Text {
                                id: unreadText
                                anchors.centerIn: parent
                                text: Number(modelData.unread || 0) > 99
                                  ? "99+" : String(modelData.unread || 0)
                                color: root.background
                                font.family: root.fontFamily
                                font.pixelSize: Style.font.caption
                                font.bold: true
                              }
                            }
                            Item {
                              id: timeLabel
                              anchors.right: parent.right
                              anchors.verticalCenter: parent.verticalCenter
                              readonly property string shortcutLabel:
                                root.chatShortcutLabelForIndex(index)
                              readonly property string timestampLabel:
                                Model.shortTime(modelData.last_timestamp)
                              width: Math.max(timestampText.implicitWidth,
                                shortcutText.implicitWidth)
                              height: Math.max(timestampText.implicitHeight,
                                shortcutText.implicitHeight)

                              Text {
                                id: timestampText
                                anchors.right: parent.right
                                anchors.verticalCenter: parent.verticalCenter
                                visible: !shortcutText.visible
                                text: timeLabel.timestampLabel
                                color: root.sidebarSecondary
                                font.family: root.fontFamily
                                font.pixelSize: Style.font.caption
                                maximumLineCount: 1
                                wrapMode: Text.NoWrap
                              }
                              Text {
                                id: shortcutText
                                anchors.right: parent.right
                                anchors.verticalCenter: parent.verticalCenter
                                visible: root.controlHeld
                                  && timeLabel.shortcutLabel !== ""
                                text: timeLabel.shortcutLabel
                                color: root.sidebarSecondary
                                font.family: root.fontFamily
                                font.pixelSize: Style.font.caption
                                maximumLineCount: 1
                                wrapMode: Text.NoWrap
                              }
                            }
                          }
                          Text {
                            width: parent.width
                            text: String(modelData.last_message || "No messages yet")
                            color: root.sidebarSecondary
                            font.family: root.fontFamily
                            font.pixelSize: Style.font.caption
                            maximumLineCount: 1
                            wrapMode: Text.NoWrap
                            elide: Text.ElideRight
                          }
                        }
                      }

                      MouseArea {
                        id: rowMouse
                        anchors.fill: parent
                        hoverEnabled: true
                        cursorShape: Qt.PointingHandCursor
                        onClicked: root.chooseChat(modelData.jid)
                      }
                    }

                    Text {
                      anchors.centerIn: parent
                      visible: chatList.count === 0
                      text: root.unreadOnly
                        ? (chatSearch.text
                          ? "No matching unread conversations"
                          : "No unread conversations")
                        : (chatSearch.text
                          ? "No matching conversations" : "No conversations yet")
                      color: root.muted
                      font.family: root.fontFamily
                      font.pixelSize: Style.font.body
                    }
                  }
                }

                Rectangle {
                  anchors.top: parent.top
                  anchors.bottom: parent.bottom
                  anchors.right: parent.right
                  width: Math.max(1, Style.normalBorderWidth)
                  color: Style.normalBorderFor(root.foreground, root.accent)
                }
              }

              Item {
                id: conversation
                width: parent.width - sidebar.width
                height: parent.height

                Column {
                  anchors.fill: parent
                  spacing: 0

                  Item {
                    id: conversationHeader
                    width: parent.width
                    height: Style.space(62)
                    Row {
                      anchors.left: parent.left
                      anchors.leftMargin: Style.space(18)
                      anchors.verticalCenter: parent.verticalCenter
                      spacing: Style.space(10)
                      CrispBorderSurface {
                        width: Style.space(38)
                        height: width
                        radius: width / 2
                        clip: true
                        color: Style.normalFillFor(root.foreground, root.accent)
                        sourceBorderSpec: Border.flat(
                          Style.normalBorderFor(root.foreground, root.accent),
                          Math.max(1, Style.normalBorderWidth))
                        Text {
                          anchors.centerIn: parent
                          visible: String(selectedAvatar.source) === ""
                            || selectedAvatar.status === Image.Error
                          text: root.service && root.service.selectedChat
                            ? Model.initials(root.service.selectedChat.name,
                              root.service.selectedChat.jid) : "?"
                          color: root.foreground
                          font.family: root.fontFamily
                          font.pixelSize: Style.font.caption
                          font.bold: true
                        }
                        Rectangle {
                          id: selectedAvatarMask
                          anchors.fill: parent
                          radius: width / 2
                          visible: false
                          layer.enabled: true
                        }
                        Image {
                          id: selectedAvatar
                          anchors.fill: parent
                          source: root.service && root.service.selectedChat
                            ? root.service.avatarUrl(root.service.selectedChat.jid) : ""
                          asynchronous: true
                          cache: false
                          fillMode: Image.PreserveAspectCrop
                          layer.enabled: true
                          layer.smooth: true
                          layer.effect: MultiEffect {
                            maskEnabled: true
                            maskSource: selectedAvatarMask
                            maskThresholdMin: 0.5
                            maskSpreadAtMin: 1.0
                          }
                        }
                      }
                      Column {
                        anchors.verticalCenter: parent.verticalCenter
                        spacing: Style.space(2)
                        Text {
                          text: root.service && root.service.selectedChat
                            ? Model.friendlyName(root.service.selectedChat.name,
                              root.service.selectedChat.jid) : "Select a conversation"
                          color: root.foreground
                          font.family: root.fontFamily
                          font.pixelSize: Style.font.subtitle
                          font.bold: true
                        }
                        Text {
                          id: conversationSubtitle
                          visible: root.service && root.service.selectedChat
                            && conversationSubtitle.text !== ""
                          text: !root.service || !root.service.selectedChat
                            ? "" : root.service.selectedChat.is_group
                              ? "Group conversation"
                              : Model.contactPhoneNumber(
                                root.service.selectedChat.phone_number,
                                root.service.selectedChat.jid)
                          color: root.foreground
                          font.family: root.fontFamily
                          font.pixelSize: Style.font.caption
                          font.underline: conversationPhoneMouse.containsMouse

                          MouseArea {
                            id: conversationPhoneMouse
                            anchors.fill: parent
                            enabled: root.service && root.service.selectedChat
                              && !root.service.selectedChat.is_group
                              && conversationSubtitle.text !== ""
                            hoverEnabled: true
                            cursorShape: enabled ? Qt.PointingHandCursor : Qt.ArrowCursor

                            onClicked: Quickshell.execDetached([
                              "bash", "-c",
                              "printf %s \"$1\" | wl-copy"
                                + " && omarchy-notification-send -g 󰅍 -t 2000"
                                + " \"Phone number copied\" \"$1\"",
                              "bash", conversationSubtitle.text
                            ])
                          }
                        }
                      }
                    }
                    Rectangle {
                      anchors.left: parent.left
                      anchors.right: parent.right
                      anchors.bottom: parent.bottom
                      height: Math.max(1, Style.normalBorderWidth)
                      color: Style.normalBorderFor(root.foreground, root.accent)
                    }
                  }

                  ListView {
                    id: messageList
                    width: parent.width
                    height: parent.height - conversationHeader.height - composerRow.height
                    clip: true
                    opacity: root.conversationReady
                      || !(root.service && root.service.selectedChatJid) ? 1 : 0
                    interactive: root.conversationReady
                      || !(root.service && root.service.selectedChatJid)
                    model: root.service ? root.service.messages : []
                    spacing: Style.space(4)
                    topMargin: Style.space(12)
                    bottomMargin: Style.space(12)
                    boundsBehavior: Flickable.StopAtBounds
                    reuseItems: true
                    QQC.ScrollBar.vertical: QQC.ScrollBar {}

                    delegate: Item {
                      id: messageDelegate
                      required property var modelData
                      required property int index
                      readonly property var mediaData: modelData.media || null
                      readonly property var reactionsData: modelData.reactions || []
                      readonly property int reactionCount: reactionsData
                        && typeof reactionsData.length === "number"
                        ? reactionsData.length : 0
                      readonly property bool hasStructuredMedia: mediaData
                        && (mediaData.kind === "image"
                          || mediaData.kind === "document"
                          || mediaData.kind === "location")
                      readonly property bool hasMediaCaption: mediaData
                        && String(modelData.text || "").charAt(0) !== "["
                      readonly property bool showSenderLabel: root.service
                        && root.service.selectedChat
                        && root.service.selectedChat.is_group === true
                      readonly property string senderLabelText: modelData.from_me
                        ? "Me" : Model.friendlyName(modelData.sender_name,
                          modelData.sender_jid)
                      readonly property string renderedMessageText:
                        Model.linkifiedMessage(modelData.text, root.accent)
                      readonly property bool showSenderAvatar: !modelData.from_me
                        && showSenderLabel
                      readonly property var nextMessage: root.service
                        && index + 1 < root.service.messages.length
                        ? root.service.messages[index + 1] : null
                      readonly property bool showMessageTime: !nextMessage
                        || !sameSender(nextMessage)
                        || !sameMinute(nextMessage)
                      property real reactionPickerX: 0
                      property real reactionPickerY: 0

                      function sameSender(otherMessage) {
                        if (!otherMessage) return false
                        if (modelData.from_me || otherMessage.from_me)
                          return modelData.from_me === true
                            && otherMessage.from_me === true
                        return String(modelData.sender_jid
                          || modelData.sender_name || "")
                          === String(otherMessage.sender_jid
                            || otherMessage.sender_name || "")
                      }

                      function sameMinute(otherMessage) {
                        return Math.floor(Number(modelData.timestamp || 0) / 60)
                          === Math.floor(Number(otherMessage.timestamp || 0) / 60)
                      }

                      function ownReactionEmoji() {
                        for (var i = 0; i < reactionsData.length; i++)
                          if (reactionsData[i].from_me === true)
                            return String(reactionsData[i].emoji || "")
                        return ""
                      }

                      function toggleReaction(emoji) {
                        if (!root.service) return
                        root.service.reactToMessage(modelData,
                          ownReactionEmoji() === emoji ? "" : emoji)
                        reactionPicker.close()
                      }

                      function openReactionPicker(x, y) {
                        reactionPickerX = x
                        reactionPickerY = y
                        reactionPicker.open()
                      }

                      width: messageList.width
                      height: Math.max(bubble.height, senderAvatar.height)
                        + (reactionsBar.visible
                          ? reactionsBar.height - Style.space(6) : 0)
                        + (messageFooterTime.visible
                          ? messageFooterTime.implicitHeight + Style.space(3) : 0)
                        + Style.space(4)

                      ListView.onPooled: reactionPicker.close()
                      ListView.onReused: reactionPicker.close()

                      CrispBorderSurface {
                        id: senderAvatar
                        visible: messageDelegate.showSenderAvatar
                        width: visible ? Style.space(30) : 0
                        height: width
                        x: Style.space(16)
                        anchors.verticalCenter: bubble.verticalCenter
                        radius: width / 2
                        clip: true
                        color: Style.normalFillFor(root.foreground, root.accent)
                        sourceBorderSpec: Border.flat(
                          Style.normalBorderFor(root.foreground, root.accent),
                          Math.max(1, Style.normalBorderWidth))
                        Text {
                          anchors.centerIn: parent
                          visible: senderAvatarImage.status !== Image.Ready
                          text: Model.initials(modelData.sender_name, modelData.sender_jid)
                          color: root.foreground
                          font.family: root.fontFamily
                          font.pixelSize: Style.font.caption
                          font.bold: true
                        }
                        Rectangle {
                          id: senderAvatarMask
                          anchors.fill: parent
                          radius: width / 2
                          visible: false
                          layer.enabled: true
                        }
                        Image {
                          id: senderAvatarImage
                          anchors.fill: parent
                          source: root.service
                            ? root.service.avatarUrl(modelData.sender_jid) : ""
                          asynchronous: true
                          cache: false
                          fillMode: Image.PreserveAspectCrop
                          layer.enabled: true
                          layer.smooth: true
                          layer.effect: MultiEffect {
                            maskEnabled: true
                            maskSource: senderAvatarMask
                            maskThresholdMin: 0.5
                            maskSpreadAtMin: 1.0
                          }
                        }
                      }

                      Text {
                        id: messageWidthProbe
                        visible: false
                        text: messageDelegate.renderedMessageText
                        font.family: root.fontFamily
                        font.pixelSize: Style.font.body
                        wrapMode: Text.NoWrap
                        textFormat: Text.StyledText
                      }

                      Text {
                        id: senderWidthProbe
                        visible: false
                        text: messageDelegate.senderLabelText
                        font.family: root.fontFamily
                        font.pixelSize: root.messageMetaFontSize
                        font.bold: true
                      }

                      Rectangle {
                        id: bubble
                        readonly property real horizontalPadding: Style.space(22)
                        readonly property real maximumWidth: messageList.width * 0.72

                        width: messageDelegate.hasStructuredMedia
                          ? Math.min(maximumWidth, Style.space(340))
                          : Math.min(maximumWidth,
                            Math.max(Style.space(36),
                              messageWidthProbe.implicitWidth + horizontalPadding,
                              messageDelegate.showSenderLabel
                                ? senderWidthProbe.implicitWidth
                                  + horizontalPadding : 0))
                        height: messageColumn.implicitHeight + Style.space(16)
                        x: modelData.from_me
                          ? messageDelegate.width - width - Style.space(18)
                          : (messageDelegate.showSenderAvatar
                            ? Style.space(56) : Style.space(18))
                        radius: Style.cornerRadius + Style.space(6)
                        color: modelData.from_me
                          ? Style.selectedFillFor(root.foreground, root.accent)
                          : Style.normalFillFor(root.foreground, root.accent)

                        Column {
                          id: messageColumn
                          anchors.left: parent.left
                          anchors.right: parent.right
                          anchors.leftMargin: Style.space(11)
                          anchors.rightMargin: Style.space(11)
                          anchors.verticalCenter: parent.verticalCenter
                          spacing: Style.space(3)
                          Text {
                            id: senderLabel
                            visible: messageDelegate.showSenderLabel
                            width: parent.width
                            text: messageDelegate.senderLabelText
                            color: root.accent
                            font.family: root.fontFamily
                            font.pixelSize: root.messageMetaFontSize
                            font.bold: true
                            elide: Text.ElideRight
                            horizontalAlignment: modelData.from_me
                              ? Text.AlignRight : Text.AlignLeft
                          }
                          Text {
                            id: messageText
                            visible: !messageDelegate.mediaData
                            width: parent.width
                            text: messageDelegate.renderedMessageText
                            color: root.foreground
                            font.family: root.fontFamily
                            font.pixelSize: Style.font.body
                            wrapMode: Text.WrapAtWordBoundaryOrAnywhere
                            textFormat: Text.StyledText
                            horizontalAlignment: modelData.from_me
                              ? Text.AlignRight : Text.AlignLeft

                            onLinkActivated: function (link) {
                              Qt.openUrlExternally(link)
                            }

                            HoverHandler {
                              enabled: messageText.hoveredLink !== ""
                              cursorShape: Qt.PointingHandCursor
                            }
                          }
                          Text {
                            id: mediaCaptionText
                            visible: messageDelegate.hasMediaCaption
                            width: parent.width
                            text: messageDelegate.renderedMessageText
                            color: root.foreground
                            font.family: root.fontFamily
                            font.pixelSize: Style.font.body
                            wrapMode: Text.WrapAtWordBoundaryOrAnywhere
                            textFormat: Text.StyledText
                            horizontalAlignment: modelData.from_me
                              ? Text.AlignRight : Text.AlignLeft

                            onLinkActivated: function (link) {
                              Qt.openUrlExternally(link)
                            }

                            HoverHandler {
                              enabled: mediaCaptionText.hoveredLink !== ""
                              cursorShape: Qt.PointingHandCursor
                            }
                          }
                          Item {
                            id: imageCard
                            visible: messageDelegate.mediaData
                              && messageDelegate.mediaData.kind === "image"
                            width: parent.width
                            height: visible ? Math.max(Style.space(110), Math.min(
                              Style.space(280), width * Number(
                                messageDelegate.mediaData.height || 1)
                                / Math.max(1, Number(messageDelegate.mediaData.width || 1)))) : 0

                            Image {
                              anchors.fill: parent
                              source: messageDelegate.mediaData
                                && imageCard.visible && root.service
                                ? root.service.fileUrl(
                                  messageDelegate.mediaData.downloaded === true
                                    ? messageDelegate.mediaData.path
                                    : (messageDelegate.mediaData.thumbnail_path
                                      || messageDelegate.mediaData.path)) : ""
                              asynchronous: true
                              cache: false
                              fillMode: Image.PreserveAspectFit
                            }

                            CrispButton {
                              readonly property bool downloading: visible
                                && root.service
                                && root.service.imageDownloading(modelData)

                              anchors.centerIn: parent
                              visible: messageDelegate.mediaData
                                && imageCard.visible
                                && messageDelegate.mediaData.downloaded !== true
                              width: Style.space(40)
                              height: Style.space(40)
                              iconText: downloading ? "󰔟" : "󰇚"
                              tooltipText: downloading
                                ? "Downloading full image"
                                : "Download full image"
                              foreground: root.foreground
                              accent: root.accent
                              enabled: visible && root.service && !downloading
                              onClicked: root.service.downloadImage(modelData)
                            }
                          }
                          Item {
                            id: documentCard
                            visible: messageDelegate.mediaData
                              && messageDelegate.mediaData.kind === "document"
                            width: parent.width
                            height: visible ? Math.max(
                              documentIcon.implicitHeight,
                              documentTextColumn.implicitHeight,
                              Style.space(34)) : 0

                            Text {
                              id: documentIcon
                              anchors.left: parent.left
                              anchors.verticalCenter: parent.verticalCenter
                              text: "󰈙"
                              color: root.muted
                              font.family: root.fontFamily
                              font.pixelSize: Style.font.displayLarge
                            }
                            Column {
                              id: documentTextColumn
                              anchors.left: documentIcon.right
                              anchors.right: documentOpenButton.left
                              anchors.leftMargin: Style.space(10)
                              anchors.rightMargin: Style.space(8)
                              anchors.verticalCenter: parent.verticalCenter
                              spacing: Style.space(3)
                              Text {
                                width: parent.width
                                text: String(messageDelegate.mediaData
                                  ? messageDelegate.mediaData.file_name || "Document"
                                  : "Document")
                                color: root.foreground
                                font.family: root.fontFamily
                                font.pixelSize: Style.font.body
                                font.bold: true
                                elide: Text.ElideMiddle
                              }
                              Text {
                                width: parent.width
                                text: messageDelegate.mediaData
                                  ? Model.documentDetails(
                                    messageDelegate.mediaData.mime_type,
                                    messageDelegate.mediaData.file_size,
                                    messageDelegate.mediaData.page_count) : "Document"
                                color: root.foreground
                                font.family: root.fontFamily
                                font.pixelSize: Style.font.caption
                                elide: Text.ElideRight
                              }
                            }
                            CrispButton {
                              id: documentOpenButton
                              anchors.right: documentSaveButton.left
                              anchors.rightMargin: Style.space(4)
                              anchors.verticalCenter: parent.verticalCenter
                              width: Style.space(34)
                              height: Style.space(34)
                              iconText: "󰏌"
                              tooltipText: "Open document"
                              foreground: root.foreground
                              accent: root.accent
                              bordered: false
                              onClicked: if (root.service) root.service.openFile(
                                messageDelegate.mediaData.path)
                            }
                            CrispButton {
                              id: documentSaveButton
                              anchors.right: parent.right
                              anchors.verticalCenter: parent.verticalCenter
                              width: Style.space(34)
                              height: Style.space(34)
                              iconText: "󰇚"
                              tooltipText: "Save to Downloads"
                              foreground: root.foreground
                              accent: root.accent
                              bordered: false
                              onClicked: if (root.service) root.service.saveFile(
                                messageDelegate.mediaData.path,
                                messageDelegate.mediaData.file_name)
                            }
                          }
                          CrispBorderSurface {
                            id: locationCard
                            visible: messageDelegate.mediaData
                              && messageDelegate.mediaData.kind === "location"
                            width: parent.width
                            height: visible ? Style.space(150) : 0
                            radius: Style.cornerRadius
                            clip: true
                            color: Style.hoverFillFor(root.foreground, root.accent)
                            sourceBorderSpec: Border.flat(
                              Style.normalBorderFor(root.foreground, root.accent),
                              Math.max(1, Style.normalBorderWidth))

                            Image {
                              anchors.fill: parent
                              source: visible && root.service
                                ? root.service.fileUrl(messageDelegate.mediaData.thumbnail_path) : ""
                              asynchronous: true
                              cache: false
                              fillMode: Image.PreserveAspectCrop
                              opacity: status === Image.Ready ? 0.42 : 0
                            }
                            Column {
                              anchors.left: parent.left
                              anchors.right: parent.right
                              anchors.bottom: parent.bottom
                              anchors.margins: Style.space(10)
                              spacing: Style.space(2)
                              Text {
                                width: parent.width
                                text: messageDelegate.mediaData
                                  && messageDelegate.mediaData.live
                                  ? "󰍹  Live location" : "󰍎  Location"
                                color: root.foreground
                                font.family: root.fontFamily
                                font.pixelSize: Style.font.body
                                font.bold: true
                              }
                              Text {
                                width: parent.width
                                text: String(messageDelegate.mediaData
                                  ? (messageDelegate.mediaData.name
                                  || messageDelegate.mediaData.address)
                                  || "Open in map"
                                  : "Open in map")
                                color: root.foreground
                                font.family: root.fontFamily
                                font.pixelSize: Style.font.caption
                                elide: Text.ElideRight
                              }
                              Text {
                                width: parent.width
                                text: Number(messageDelegate.mediaData
                                  ? messageDelegate.mediaData.accuracy_m || 0 : 0) > 0
                                  ? "Accuracy ±" + Number(messageDelegate.mediaData.accuracy_m) + " m"
                                  : "OpenStreetMap"
                                color: root.muted
                                font.family: root.fontFamily
                                font.pixelSize: Style.font.caption
                              }
                            }
                            MouseArea {
                              anchors.fill: parent
                              cursorShape: Qt.PointingHandCursor
                              onClicked: if (root.service) root.service.openMap(
                                messageDelegate.mediaData.latitude_e7,
                                messageDelegate.mediaData.longitude_e7)
                            }
                          }
                        }

                        MouseArea {
                          anchors.fill: parent
                          acceptedButtons: Qt.RightButton
                          onPressed: function(mouse) {
                            messageDelegate.openReactionPicker(mouse.x, mouse.y)
                            mouse.accepted = true
                          }
                        }
                      }

                      Row {
                        id: reactionsBar
                        visible: messageDelegate.reactionCount > 0
                        height: Style.space(28)
                        width: implicitWidth
                        spacing: Style.space(4)
                        anchors.top: bubble.bottom
                        anchors.topMargin: -Style.space(6)
                        x: modelData.from_me
                          ? bubble.x + bubble.width - width : bubble.x
                        z: 2

                        Repeater {
                          model: messageDelegate.reactionsData
                          delegate: CrispBorderSurface {
                            required property var modelData
                            width: reactionLabel.implicitWidth + Style.space(14)
                            height: reactionsBar.height
                            radius: height / 2
                            color: modelData.from_me
                              ? Style.selectedFillFor(root.foreground, root.accent)
                              : Style.hoverFillFor(root.foreground, root.accent)
                            sourceBorderSpec: Border.flat(
                              modelData.from_me ? root.accent
                                : Style.normalBorderFor(root.foreground, root.accent),
                              Math.max(1, Style.normalBorderWidth))
                            Text {
                              id: reactionLabel
                              anchors.centerIn: parent
                              text: String(modelData.emoji || "")
                                + (Number(modelData.count || 0) > 1
                                  ? "  " + Number(modelData.count) : "")
                              color: root.foreground
                              font.family: root.fontFamily
                              font.pixelSize: Style.font.body
                            }
                            MouseArea {
                              anchors.fill: parent
                              cursorShape: Qt.PointingHandCursor
                              onClicked: messageDelegate.toggleReaction(
                                String(modelData.emoji || ""))
                            }
                          }
                        }

                          QQC.Popup {
                            id: reactionPicker

                            property bool waitingForEmojiPicker: false
                            readonly property var popupBorderSpec:
                              Border.localOrSurfaceSpec("popups", "border",
                                Color.popups.border, Color.popups.border,
                                Math.max(1, Style.normalBorderWidth))

                            function openOmarchyEmojiPicker() {
                              waitingForEmojiPicker = true
                              emojiPasteTarget.text = ""
                              emojiPasteTarget.forceActiveFocus()
                              Qt.callLater(function() {
                                if (!root.shell
                                    || typeof root.shell.summon !== "function"
                                    || !root.shell.summon("omarchy.emojis", "{}"))
                                  reactionPicker.waitingForEmojiPicker = false
                              })
                            }

                            function acceptEmojiPickerText() {
                              var value = emojiPasteTarget.text.trim()
                              if (!waitingForEmojiPicker || !value) return
                              waitingForEmojiPicker = false
                              Qt.callLater(function() {
                                messageDelegate.toggleReaction(value)
                              })
                            }

                            parent: bubble
                            x: modelData.from_me
                              ? messageDelegate.reactionPickerX - width
                              : messageDelegate.reactionPickerX
                            y: messageDelegate.reactionPickerY + Style.space(4)
                            width: Style.space(328)
                            height: reactionPickerColumn.implicitHeight
                              + topPadding + bottomPadding
                            margins: Style.space(8)
                            padding: Style.space(10)
                            leftPadding: padding + Border.left(popupBorderSpec)
                            rightPadding: padding + Border.right(popupBorderSpec)
                            topPadding: padding + Border.top(popupBorderSpec)
                            bottomPadding: padding + Border.bottom(popupBorderSpec)
                            modal: false
                            focus: true
                            closePolicy: QQC.Popup.CloseOnEscape
                              | QQC.Popup.CloseOnPressOutsideParent

                            onOpened: {
                              waitingForEmojiPicker = false
                              emojiPasteTarget.text = ""
                            }
                            onClosed: waitingForEmojiPicker = false

                            background: CrispBorderSurface {
                              color: Color.popups.background
                              sourceBorderSpec: reactionPicker.popupBorderSpec
                              radius: Style.cornerRadius + Style.space(4)
                            }

                            contentItem: Column {
                              id: reactionPickerColumn
                              spacing: Style.space(8)

                              Item {
                                width: parent.width
                                height: Math.max(reactionTitle.implicitHeight,
                                  reactionHint.implicitHeight)

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
                                  anchors.right: parent.right
                                  anchors.verticalCenter: parent.verticalCenter
                                  text: messageDelegate.ownReactionEmoji() !== ""
                                    ? "Tap selected to remove" : "Choose one"
                                  color: root.muted
                                  font.family: root.fontFamily
                                  font.pixelSize: root.messageMetaFontSize
                                }
                              }

                              Row {
                                id: quickReactionRow
                                width: parent.width
                                spacing: Style.space(6)

                                Repeater {
                                  model: ["👍", "❤️", "😂", "😮", "😢", "🙏"]

                                  delegate: CrispBorderSurface {
                                    id: quickReaction
                                    required property string modelData
                                    readonly property bool selected:
                                      messageDelegate.ownReactionEmoji() === modelData
                                    readonly property bool hot: quickReactionHover.hovered

                                    width: (quickReactionRow.width
                                      - quickReactionRow.spacing * 5) / 6
                                    height: width
                                    radius: width / 2
                                    color: selected
                                      ? Style.selectedFillFor(root.foreground, root.accent)
                                      : hot
                                        ? Style.hoverFillFor(root.foreground, root.accent)
                                        : "transparent"
                                    sourceBorderSpec: selected
                                      ? Border.controlSpec("selected",
                                        root.foreground, root.accent)
                                      : hot
                                        ? Border.controlSpec("hover-cursor",
                                          root.foreground, root.accent)
                                        : Border.none()

                                    Behavior on color {
                                      ColorAnimation { duration: 100 }
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
                                    HoverHandler { id: quickReactionHover }
                                    MouseArea {
                                      anchors.fill: parent
                                      cursorShape: Qt.PointingHandCursor
                                      onClicked: messageDelegate.toggleReaction(
                                        quickReaction.modelData)
                                    }
                                  }
                                }
                              }

                              Rectangle {
                                width: parent.width
                                height: Math.max(1, Style.normalBorderWidth)
                                color: Style.normalBorderFor(
                                  root.foreground, root.accent)
                              }

                              Item {
                                width: parent.width
                                height: Style.space(36)

                                CrispTextField {
                                  id: emojiPasteTarget
                                  anchors.left: parent.left
                                  anchors.bottom: parent.bottom
                                  width: 1
                                  height: 1
                                  opacity: 0
                                  activeFocusOnTab: false
                                  onTextChanged:
                                    reactionPicker.acceptEmojiPickerText()
                                }
                                CrispButton {
                                  anchors.fill: parent
                                  iconText: ""
                                  text: reactionPicker.waitingForEmojiPicker
                                    ? "Choose an emoji…" : "Choose any emoji"
                                  tooltipText: "Open the Omarchy emoji picker"
                                  foreground: Color.popups.text
                                  accent: root.accent
                                  bordered: true
                                  onClicked:
                                    reactionPicker.openOmarchyEmojiPicker()
                                }
                              }
                            }
                          }
                      }

                      Text {
                        id: messageFooterTime
                        visible: messageDelegate.showMessageTime
                        anchors.top: reactionsBar.visible
                          ? reactionsBar.bottom : bubble.bottom
                        anchors.topMargin: Style.space(3)
                        x: modelData.from_me
                          ? bubble.x + bubble.width - width - Style.space(4)
                          : bubble.x + Style.space(4)
                        text: Model.messageTime(modelData.timestamp)
                        color: root.timestamp
                        font.family: root.fontFamily
                        font.pixelSize: root.messageMetaFontSize
                      }
                    }

                    Column {
                      anchors.centerIn: parent
                      visible: messageList.count === 0
                      spacing: Style.space(8)
                      Image {
                        visible: !(root.service && root.service.selectedChatJid)
                        anchors.horizontalCenter: parent.horizontalCenter
                        width: Style.font.displayLarge
                        height: width
                        source: root.whatsappIcon
                        sourceSize: Qt.size(width, height)
                        fillMode: Image.PreserveAspectFit
                        smooth: true
                        layer.enabled: true
                        layer.smooth: true
                        layer.effect: MultiEffect {
                          colorization: 1
                          colorizationColor: root.muted
                        }
                      }
                      Text {
                        anchors.horizontalCenter: parent.horizontalCenter
                        text: "󰍡"
                        color: root.muted
                        font.family: root.fontFamily
                        font.pixelSize: Style.font.displayLarge
                        visible: root.service && root.service.selectedChatJid
                      }
                      Text {
                        anchors.horizontalCenter: parent.horizontalCenter
                        text: root.service && root.service.selectedChatJid
                          ? "No messages in this conversation" : "Choose a conversation"
                        color: root.muted
                        font.family: root.fontFamily
                        font.pixelSize: Style.font.body
                      }
                    }
                  }

                  Item {
                    id: composerRow
                    width: parent.width
                    height: Style.space(66)
                    Rectangle {
                      anchors.left: parent.left
                      anchors.right: parent.right
                      anchors.top: parent.top
                      height: Math.max(1, Style.normalBorderWidth)
                      color: Style.normalBorderFor(root.foreground, root.accent)
                    }
                    Row {
                      anchors.left: parent.left
                      anchors.right: parent.right
                      anchors.leftMargin: Style.space(14)
                      anchors.rightMargin: Style.space(14)
                      anchors.verticalCenter: parent.verticalCenter
                      spacing: Style.space(8)
                      CrispTextField {
                        id: composer
                        width: parent.width - sendButton.width - parent.spacing
                        enabled: root.service && root.service.selectedChatJid !== ""
                        placeholderText: enabled ? "Message" : "Select a conversation"
                        onAccepted: root.submitMessage()
                      }
                      CrispButton {
                        id: sendButton
                        iconText: "󰒊"
                        text: "Send"
                        bordered: true
                        active: composer.text.trim() !== ""
                        enabled: composer.enabled && composer.text.trim() !== ""
                        foreground: root.foreground
                        onClicked: root.submitMessage()
                      }
                    }
                  }
                }
              }
            }
          }

          Item {
            anchors.fill: parent
            visible: !root.paired
            Column {
              anchors.centerIn: parent
              width: Math.min(parent.width - Style.space(48), Style.space(520))
              spacing: Style.space(14)
              Text {
                anchors.horizontalCenter: parent.horizontalCenter
                text: root.pairing ? "Link WhatsApp" : "Connecting to WhatsApp"
                color: root.foreground
                font.family: root.fontFamily
                font.pixelSize: Style.font.displayLarge
                font.bold: true
              }
              Text {
                anchors.horizontalCenter: parent.horizontalCenter
                width: parent.width
                horizontalAlignment: Text.AlignHCenter
                text: root.pairing
                  ? "On your phone, open WhatsApp → Linked devices → Link a device"
                  : (root.service && root.service.connectionDetail
                    ? root.service.connectionDetail : "The background service is starting…")
                color: root.muted
                font.family: root.fontFamily
                font.pixelSize: Style.font.body
                wrapMode: Text.Wrap
              }
              CrispBorderSurface {
                visible: root.pairing
                anchors.horizontalCenter: parent.horizontalCenter
                width: Style.space(340)
                height: width
                radius: Style.cornerRadius
                color: "white"
                sourceBorderSpec: Border.flat(root.accent,
                  Math.max(1, Style.normalBorderWidth))
                Image {
                  anchors.fill: parent
                  anchors.margins: Style.space(14)
                  source: root.service ? root.service.qrImageUrl : ""
                  fillMode: Image.PreserveAspectFit
                  cache: false
                  asynchronous: true
                }
              }
              Text {
                visible: root.pairing
                anchors.horizontalCenter: parent.horizontalCenter
                width: parent.width
                horizontalAlignment: Text.AlignHCenter
                text: "Your linked-device keys and recent text history stay on this computer."
                color: root.muted
                font.family: root.fontFamily
                font.pixelSize: Style.font.caption
                wrapMode: Text.Wrap
              }
              CrispButton {
                visible: !root.pairing
                anchors.horizontalCenter: parent.horizontalCenter
                text: "Try again"
                iconText: "󰑐"
                bordered: true
                foreground: root.foreground
                onClicked: if (root.service) root.service.refresh()
              }
            }
          }

          CrispBorderSurface {
            visible: root.service && root.service.lastError !== ""
            anchors.left: parent.left
            anchors.right: parent.right
            anchors.bottom: parent.bottom
            anchors.leftMargin: Style.space(18)
            anchors.rightMargin: Style.space(18)
            anchors.bottomMargin: Style.space(12)
            height: errorText.implicitHeight + Style.space(16)
            radius: Style.cornerRadius
            color: Style.selectedFillFor(root.foreground, Color.urgent)
            sourceBorderSpec: Border.flat(Color.urgent,
              Math.max(1, Style.normalBorderWidth))
            Text {
              id: errorText
              anchors.fill: parent
              anchors.margins: Style.space(8)
              text: root.service ? root.service.lastError : ""
              color: root.foreground
              font.family: root.fontFamily
              font.pixelSize: Style.font.caption
              wrapMode: Text.Wrap
            }
          }
        }
      }
    }
  }
}
