import QtQuick
import QtQuick.Controls as QQC
import QtQuick.Effects
import QtMultimedia
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
  property bool restoreConversationAfterMessages: false
  property real preservedConversationContentY: 0
  property var activeInlineVideoCard: null
  property bool activeInlineVideoGif: false
  property var activeVoiceMessageCard: null
  property real currentTimestamp: Date.now() / 1000
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
  readonly property string messageTimeFormat: Model.timeFormat(root.clockFormats())
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

  function clockFormats() {
    var config = shell && shell.shellConfig ? shell.shellConfig : null
    var registry = shell && shell.pluginRegistry ? shell.pluginRegistry : null
    if (!config || !config.bar || !config.bar.layout || !registry
        || typeof registry.findRelativeBarLocation !== "function")
      return ["dddd HH:mm"]

    var location = registry.findRelativeBarLocation(config, "omarchy.clock", "")
    if (!location || location.found !== true) return ["dddd HH:mm"]
    var entries = config.bar.layout[location.section]
    var entry = entries && entries[location.index] ? entries[location.index] : null
    if (!entry) return ["dddd HH:mm"]

    var vertical = shell.bar && shell.bar.vertical === true
    return vertical
      ? [entry.verticalFormat, entry.format, entry.verticalFormatAlt, entry.formatAlt]
      : [entry.format, entry.verticalFormat, entry.formatAlt, entry.verticalFormatAlt]
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

  component CrispMenuButton: CrispButton {
    id: menuButton

    property string menuIconText: ""
    property string menuText: ""

    iconText: ""
    text: ""
    implicitHeight: menuButtonContent.implicitHeight + verticalPadding * 2
      + _reservedBorderTop + _reservedBorderBottom

    Row {
      id: menuButtonContent

      anchors.left: parent.left
      anchors.leftMargin: menuButton._reservedContentLeftInset
      anchors.verticalCenter: parent.verticalCenter
      spacing: Style.spacing.controlGap

      Item {
        width: Style.space(20)
        height: menuButtonIcon.implicitHeight

        Text {
          id: menuButtonIcon

          anchors.centerIn: parent
          text: menuButton.menuIconText
          color: menuButton.selected
            ? menuButton._selectedColor : menuButton.foreground
          font.family: menuButton.fontFamily
          font.pixelSize: menuButton.iconSize
        }
      }

      Text {
        anchors.verticalCenter: parent.verticalCenter
        text: menuButton.menuText
        color: menuButton.selected
          ? menuButton._selectedColor : menuButton.foreground
        font.family: menuButton.fontFamily
        font.pixelSize: menuButton.fontSize
        font.bold: menuButton.selected
      }
    }
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

  function groupConversationSubtitle() {
    if (!service || !service.selectedChat
        || service.selectedChat.is_group !== true) return ""
    if (service.groupParticipantsChatJid !== service.selectedChatJid)
      return "Loading participants…"
    if (service.groupParticipantsError) return "Participants unavailable"
    var participants = Array.isArray(service.groupParticipants)
      ? service.groupParticipants : []
    if (!participants.length) return "Participants unavailable"
    var labels = []
    var shown = Math.min(4, participants.length)
    for (var i = 0; i < shown; i++) {
      var participant = participants[i] || {}
      labels.push(participant.is_me === true
        ? "You" : Model.friendlyName(participant.name, participant.jid))
    }
    var remaining = participants.length - shown
    return labels.join(", ") + (remaining > 0
      ? " + " + remaining + (remaining === 1 ? " other" : " others") : "")
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
      Quickshell.execDetached([
        "hyprctl", "dispatch",
        "hl.dsp.focus({ window = \"title:^WhatsApp$\" })"
      ])
      root.revealSelectedChat()
      root.prepareCurrentConversation()
      focusScope.forceActiveFocus()
      if (service && service.selectedChatJid) composer.forceActiveFocus()
    })
  }

  function close() {
    stopInlineVideo()
    stopVoiceMessage()
    scrollToBottomAnimation.stop()
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

  function chooseRelativeChat(offset) {
    if (!filteredChats.length) return false
    var selectedIndex = -1
    var selectedJid = service ? String(service.selectedChatJid || "") : ""
    for (var i = 0; i < filteredChats.length; i++) {
      if (String(filteredChats[i].jid || "") === selectedJid) {
        selectedIndex = i
        break
      }
    }
    var index = selectedIndex < 0
      ? (offset < 0 ? filteredChats.length - 1 : 0)
      : (selectedIndex + offset + filteredChats.length) % filteredChats.length
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

  function stopInlineVideo(card) {
    if (card && activeInlineVideoCard !== card) return
    inlineVideoPlayer.stop()
    inlineVideoPlayer.source = ""
    activeInlineVideoCard = null
    activeInlineVideoGif = false
  }

  function toggleInlineVideo(card) {
    if (!card || !card.downloaded || !card.mediaPath) return
    if (activeInlineVideoCard === card) {
      if (inlineVideoPlayer.playbackState === MediaPlayer.PlayingState)
        inlineVideoPlayer.pause()
      else
        inlineVideoPlayer.play()
      return
    }
    stopInlineVideo()
    stopVoiceMessage()
    activeInlineVideoCard = card
    activeInlineVideoGif = card.isGif
    inlineVideoPlayer.source = "file://" + card.mediaPath
    inlineVideoPlayer.play()
  }

  function stopVoiceMessage(card) {
    if (card && activeVoiceMessageCard !== card) return
    voiceMessagePlayer.stop()
    voiceMessagePlayer.source = ""
    activeVoiceMessageCard = null
  }

  function toggleVoiceMessage(card) {
    if (!card || !card.downloaded || !card.mediaPath) return
    if (activeVoiceMessageCard === card) {
      if (voiceMessagePlayer.playbackState === MediaPlayer.PlayingState)
        voiceMessagePlayer.pause()
      else
        voiceMessagePlayer.play()
      return
    }
    stopVoiceMessage()
    stopInlineVideo()
    activeVoiceMessageCard = card
    voiceMessagePlayer.source = "file://" + card.mediaPath
    voiceMessagePlayer.play()
  }

  function messageIndex(messageId) {
    if (!service || !messageId) return -1
    for (var i = 0; i < service.messages.length; i++)
      if (String(service.messages[i].id || "") === String(messageId)) return i
    return -1
  }

  function alignConversationViewportToBottom(serial, remainingPasses) {
    if (serial !== conversationScrollSerial || !messageList) return
    messageList.forceLayout()
    messageList.positionViewAtEnd()
    var heightRatio = Number(messageList.visibleArea.heightRatio || 0)
    var gapRatio = 1 - Number(messageList.visibleArea.yPosition || 0)
      - heightRatio
    if (heightRatio > 0 && gapRatio > 0)
      messageList.contentY += gapRatio * messageList.height / heightRatio
    if (remainingPasses > 0) {
      var nextPass = remainingPasses - 1
      Qt.callLater(function() {
        root.alignConversationViewportToBottom(serial, nextPass)
      })
    }
  }

  function animateConversationViewportToBottom() {
    if (!messageList || !messageList.count) return
    conversationScrollSerial++
    restoreConversationAfterMessages = false
    scrollToBottomAnimation.stop()
    messageList.cancelFlick()
    messageList.forceLayout()
    var heightRatio = Number(messageList.visibleArea.heightRatio || 0)
    var gapRatio = 1 - Number(messageList.visibleArea.yPosition || 0)
      - heightRatio
    if (heightRatio <= 0 || gapRatio <= 0) {
      scheduleConversationScroll("bottom", "")
      return
    }
    var distance = gapRatio * messageList.height / heightRatio
    scrollToBottomAnimation.from = messageList.contentY
    scrollToBottomAnimation.to = messageList.contentY + distance
    scrollToBottomAnimation.duration = Math.min(420,
      Math.max(180, 160 + Math.sqrt(distance) * 8))
    scrollToBottomAnimation.start()
  }

  function conversationViewportNearBottom() {
    if (!messageList || !conversationReady || !messageList.count) return true
    var heightRatio = Number(messageList.visibleArea.heightRatio || 0)
    var gapRatio = 1 - Number(messageList.visibleArea.yPosition || 0)
      - heightRatio
    if (heightRatio <= 0 || gapRatio <= 0) return true
    return gapRatio * messageList.height / heightRatio <= Style.space(48)
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
        alignConversationViewportToBottom(serial, 2)
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

  function scheduleConversationPositionRestore() {
    var serial = ++conversationScrollSerial
    Qt.callLater(function() {
      if (serial !== root.conversationScrollSerial
          || !root.restoreConversationAfterMessages || !messageList) return
      messageList.forceLayout()
      messageList.contentY = root.preservedConversationContentY
      root.restoreConversationAfterMessages = false
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

  function remainingTimeLabel(untilTimestamp) {
    var minutes = Math.max(0, Math.ceil(
      (Number(untilTimestamp || 0) - currentTimestamp) / 60))
    if (!minutes) return "Ended"
    if (minutes < 60)
      return minutes + (minutes === 1 ? " minute left" : " minutes left")
    var hours = Math.floor(minutes / 60)
    var remainder = minutes % 60
    var label = hours + (hours === 1 ? " hour" : " hours")
    if (remainder)
      label += " " + remainder
        + (remainder === 1 ? " minute" : " minutes")
    return label + " left"
  }

  Timer {
    interval: 30000
    repeat: true
    running: root.opened
    triggeredOnStart: true
    onTriggered: root.currentTimestamp = Date.now() / 1000
  }

  AudioOutput {
    id: inlineVideoAudio
    muted: root.activeInlineVideoGif
  }

  MediaPlayer {
    id: inlineVideoPlayer
    audioOutput: inlineVideoAudio
    videoOutput: root.activeInlineVideoCard
      ? root.activeInlineVideoCard.videoSurface : null
    loops: root.activeInlineVideoGif ? MediaPlayer.Infinite : MediaPlayer.Once

    onMediaStatusChanged: {
      if (mediaStatus === MediaPlayer.EndOfMedia
          && !root.activeInlineVideoGif) root.stopInlineVideo()
    }
  }

  AudioOutput {
    id: voiceMessageAudio
  }

  MediaPlayer {
    id: voiceMessagePlayer
    audioOutput: voiceMessageAudio

    onMediaStatusChanged: {
      if (mediaStatus === MediaPlayer.EndOfMedia) root.stopVoiceMessage()
    }
  }

  NumberAnimation {
    id: scrollToBottomAnimation

    target: messageList
    property: "contentY"
    easing.type: Easing.OutCubic
    onFinished: root.scheduleConversationScroll("bottom", "")
  }

  Connections {
    target: root.service
    function onSelectedChatJidChanged() {
      root.stopInlineVideo()
      root.stopVoiceMessage()
      scrollToBottomAnimation.stop()
      root.conversationScrollSerial++
      root.conversationReady = false
      root.scrollToBottomAfterMessages = false
      root.restoreConversationAfterMessages = false
      Qt.callLater(root.revealSelectedChat)
    }
    function onMessagesWillChange(preservePosition) {
      root.stopInlineVideo()
      root.stopVoiceMessage()
      if (!preservePosition || !messageList || !root.conversationReady
          || root.restoreConversationAfterMessages) return
      root.preservedConversationContentY = messageList.contentY
      root.restoreConversationAfterMessages = true
    }
    function onMessageSentSerialChanged() {
      root.scrollToBottomAfterMessages = true
    }
    function onIncomingMessageSerialChanged() {
      if (root.conversationViewportNearBottom())
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
        root.restoreConversationAfterMessages = false
        var firstUnreadId = String(root.service.messagesFirstUnreadId || "")
        root.scheduleConversationScroll(firstUnreadId ? "unread" : "bottom",
          firstUnreadId)
      } else if (root.scrollToBottomAfterMessages) {
        root.scrollToBottomAfterMessages = false
        root.restoreConversationAfterMessages = false
        root.scheduleConversationScroll("bottom", "")
      } else if (!root.conversationReady) {
        root.prepareCurrentConversation()
      } else if (root.restoreConversationAfterMessages) {
        root.scheduleConversationPositionRestore()
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
        if (logoutConfirmation.handleKey(event)) {
          event.accepted = true
          return
        }
        if (root.beginControlHold(event)
            || event.key === root.controlActivatorKey) {
          event.accepted = true
          return
        }
        if (!(event.modifiers & Qt.ControlModifier)) return
        if (event.key === Qt.Key_Down) {
          root.animateConversationViewportToBottom()
          event.accepted = true
          return
        }
        if (event.key === Qt.Key_BracketLeft
            || event.key === Qt.Key_BracketRight) {
          root.chooseRelativeChat(event.key === Qt.Key_BracketLeft ? -1 : 1)
          event.accepted = true
          return
        }
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

      ConfirmDialog {
        id: logoutConfirmation

        anchors.fill: parent
        z: 1000
        message: "Log out and unlink this workstation?\n\nThis clears its local "
          + "WhatsApp account data. You’ll need to scan a new QR code to use it again."
        confirmText: "Log out"
        background: Color.popups.background
        foreground: Color.popups.text
        selectedText: root.accent

        onOpenedChanged: if (opened) selectedIndex = 0
        onCanceled: opened = false
        onConfirmed: {
          if (root.service && root.service.unlinkDevice()) opened = false
        }
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

                  CrispMenuButton {
                    id: headerLicenseAction

                    width: parent.width
                    menuIconText: ""
                    menuText: "Licenses"
                    foreground: Color.popups.text
                    accent: root.accent
                    focusable: true
                    onClicked: {
                      headerMenu.close()
                      Qt.callLater(function() { licensesPopup.open() })
                    }
                  }

                  CrispMenuButton {
                    width: parent.width
                    visible: root.paired
                    menuIconText: "󰍃"
                    menuText: "Log out"
                    foreground: Color.popups.text
                    accent: Color.urgent
                    focusable: true
                    onClicked: {
                      headerMenu.close()
                      Qt.callLater(function() {
                        logoutConfirmation.opened = true
                      })
                    }
                  }

                  CrispMenuButton {
                    width: parent.width
                    menuIconText: "󰅖"
                    menuText: "Close"
                    foreground: Color.popups.text
                    accent: root.accent
                    focusable: true
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
                            visible: !chatAvatar.hasRenderedAvatar
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
                            property bool hasRenderedAvatar: false
                            anchors.fill: parent
                            source: root.service ? root.service.avatarUrl(modelData.jid) : ""
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
                                Model.shortTime(modelData.last_timestamp,
                                  root.messageTimeFormat)
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
                          Item {
                            width: parent.width
                            height: Math.max(chatPreview.implicitHeight,
                              chatStatusIcons.implicitHeight)
                            Text {
                              id: chatPreview
                              anchors.left: parent.left
                              anchors.right: chatStatusIcons.left
                              anchors.rightMargin: chatStatusIcons.visible
                                ? Style.space(5) : 0
                              text: {
                                var message = String(modelData.last_message || "")
                                if (!message) return "No messages yet"
                                var sender = String(modelData.last_sender_name || "")
                                return modelData.is_group === true && sender
                                  ? sender + ": " + message : message
                              }
                              color: root.sidebarSecondary
                              font.family: root.fontFamily
                              font.pixelSize: Style.font.caption
                              maximumLineCount: 1
                              wrapMode: Text.NoWrap
                              elide: Text.ElideRight
                            }
                            Row {
                              id: chatStatusIcons
                              visible: modelData.pinned === true
                                || modelData.muted === true
                              anchors.right: parent.right
                              anchors.verticalCenter: parent.verticalCenter
                              spacing: Style.space(4)
                              Text {
                                visible: modelData.pinned === true
                                text: "󰐃"
                                color: root.timestamp
                                font.family: root.fontFamily
                                font.pixelSize: 12
                              }
                              Text {
                                visible: modelData.muted === true
                                text: "󰂛"
                                color: root.timestamp
                                font.family: root.fontFamily
                                font.pixelSize: 12
                              }
                            }
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
                          visible: !selectedAvatar.hasRenderedAvatar
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
                          property bool hasRenderedAvatar: false
                          anchors.fill: parent
                          source: root.service && root.service.selectedChat
                            ? root.service.avatarUrl(root.service.selectedChat.jid) : ""
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
                              ? root.groupConversationSubtitle()
                              : Model.contactPhoneNumber(
                                root.service.selectedChat.phone_number,
                                root.service.selectedChat.jid)
                          color: root.foreground
                          font.family: root.fontFamily
                          font.pixelSize: Style.font.caption
                          width: Math.min(implicitWidth,
                            conversationHeader.width - Style.space(100))
                          maximumLineCount: 1
                          wrapMode: Text.NoWrap
                          elide: Text.ElideRight
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
                          || mediaData.kind === "video"
                          || mediaData.kind === "audio"
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

                      ListView.onPooled: {
                        root.stopInlineVideo(mediaPreviewCard)
                        root.stopVoiceMessage(voiceMessageCard)
                        reactionPicker.close()
                      }
                      ListView.onReused: {
                        root.stopVoiceMessage(voiceMessageCard)
                        reactionPicker.close()
                      }

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
                          visible: !senderAvatarImage.hasRenderedAvatar
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
                          property bool hasRenderedAvatar: false
                          anchors.fill: parent
                          source: root.service
                            ? root.service.avatarUrl(modelData.sender_jid) : ""
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

                      CrispBorderSurface {
                        id: bubble
                        readonly property bool borderOnlyMedia:
                          messageDelegate.mediaData
                          && (messageDelegate.mediaData.kind === "image"
                            || messageDelegate.mediaData.kind === "video"
                            || messageDelegate.mediaData.kind === "location")
                        readonly property color mediaBorderColor: {
                          var base = Style.normalBorderFor(
                            root.foreground, root.accent)
                          return Qt.rgba(base.r, base.g, base.b, base.a * 0.55)
                        }
                        readonly property real horizontalPadding: borderOnlyMedia
                          ? borderLeft + borderRight : Style.space(22)
                        readonly property real maximumWidth: messageList.width * 0.72
                        readonly property real maximumMediaWidth:
                          Math.min(maximumWidth, Style.space(340))
                            - horizontalPadding
                        readonly property real locationPreviewWidth:
                          Math.min(maximumMediaWidth, Style.space(260))
                        readonly property real videoAspectRatio:
                          Number(messageDelegate.mediaData
                            ? messageDelegate.mediaData.width || 1 : 1)
                            / Math.max(1, Number(messageDelegate.mediaData
                              ? messageDelegate.mediaData.height || 1 : 1))
                        readonly property real videoPreviewWidth: Math.max(
                          Style.space(40), Math.min(maximumMediaWidth,
                            Style.space(280) * videoAspectRatio))

                        width: messageDelegate.mediaData
                          && messageDelegate.mediaData.kind === "video"
                          ? videoPreviewWidth + horizontalPadding
                          : messageDelegate.mediaData
                            && messageDelegate.mediaData.kind === "location"
                          ? locationPreviewWidth + horizontalPadding
                          : messageDelegate.hasStructuredMedia
                          ? Math.min(maximumWidth, Style.space(340))
                          : Math.min(maximumWidth,
                            Math.max(Style.space(36),
                              messageWidthProbe.implicitWidth + horizontalPadding,
                              messageDelegate.showSenderLabel
                                ? senderWidthProbe.implicitWidth
                                  + horizontalPadding : 0))
                        height: messageColumn.implicitHeight
                          + (borderOnlyMedia
                            ? borderTop + borderBottom : Style.space(16))
                        x: modelData.from_me
                          ? messageDelegate.width - width - Style.space(18)
                          : (messageDelegate.showSenderAvatar
                            ? Style.space(56) : Style.space(18))
                        radius: borderOnlyMedia
                          ? 0 : Style.cornerRadius + Style.space(6)
                        color: borderOnlyMedia ? "transparent"
                          : (modelData.from_me
                            ? Style.selectedFillFor(root.foreground, root.accent)
                            : Style.normalFillFor(root.foreground, root.accent))
        sourceBorderSpec: borderOnlyMedia
            ? Border.flat(mediaBorderColor,
                          1)
            : Border.none()

                        Column {
                          id: messageColumn
                          anchors.left: parent.left
                          anchors.right: parent.right
                          anchors.leftMargin: bubble.borderOnlyMedia
                            ? bubble.borderLeft : Style.space(11)
                          anchors.rightMargin: bubble.borderOnlyMedia
                            ? bubble.borderRight : Style.space(11)
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
                            id: mediaPreviewCard
                            property alias videoSurface: inlineVideoOutput
                            readonly property bool isVideo: messageDelegate.mediaData
                              && messageDelegate.mediaData.kind === "video"
                            readonly property bool isGif: isVideo
                              && messageDelegate.mediaData.gif_playback === true
                            readonly property bool downloaded: messageDelegate.mediaData
                              ? messageDelegate.mediaData.downloaded === true : false
                            readonly property string mediaPath: messageDelegate.mediaData
                              ? String(messageDelegate.mediaData.path || "") : ""
                            readonly property string thumbnailPath: messageDelegate.mediaData
                              ? String(messageDelegate.mediaData.thumbnail_path || "") : ""
                            readonly property string displayPath: isVideo
                              ? thumbnailPath
                              : (downloaded ? mediaPath : thumbnailPath)
                            readonly property bool inlineActive:
                              root.activeInlineVideoCard === mediaPreviewCard
                            readonly property bool inlinePlaying: inlineActive
                              && inlineVideoPlayer.playbackState === MediaPlayer.PlayingState
                            visible: messageDelegate.mediaData
                              && (messageDelegate.mediaData.kind === "image"
                                || messageDelegate.mediaData.kind === "video")
                            width: parent.width
                            height: visible && messageDelegate.mediaData
                              ? (isVideo ? width / bubble.videoAspectRatio
                                : Math.max(Style.space(110), Math.min(
                                  Style.space(280), width * Number(
                                    messageDelegate.mediaData.height || 1)
                                    / Math.max(1, Number(
                                      messageDelegate.mediaData.width || 1)))))
                              : 0

                            Image {
                              id: mediaPreviewImage
                              anchors.fill: parent
                              visible: !mediaPreviewCard.inlineActive
                              source: mediaPreviewCard.visible && root.service
                                ? root.service.fileUrl(mediaPreviewCard.displayPath) : ""
                              asynchronous: true
                              cache: false
                              fillMode: Image.PreserveAspectFit
                            }

                            VideoOutput {
                              id: inlineVideoOutput
                              anchors.fill: parent
                              visible: mediaPreviewCard.inlineActive
                              fillMode: VideoOutput.PreserveAspectFit
                              endOfStreamPolicy: VideoOutput.KeepLastFrame
                            }

                            HoverHandler {
                              id: mediaPreviewHover
                            }

                            MouseArea {
                              anchors.fill: parent
                              enabled: mediaPreviewCard.isVideo
                                && mediaPreviewCard.downloaded
                              cursorShape: enabled
                                ? Qt.PointingHandCursor : Qt.ArrowCursor
                              onClicked:
                                root.toggleInlineVideo(mediaPreviewCard)
                            }

                            Text {
                              anchors.centerIn: parent
                              visible: mediaPreviewCard.isVideo
                                && !mediaPreviewCard.inlineActive
                                && mediaPreviewImage.status !== Image.Ready
                              text: "󰕧"
                              color: root.muted
                              font.family: root.fontFamily
                              font.pixelSize: Style.font.displayLarge
                            }

                            CrispButton {
                              readonly property bool downloading: visible
                                && root.service
                                && root.service.mediaDownloading(modelData)

                              anchors.centerIn: parent
                              visible: messageDelegate.mediaData
                                && mediaPreviewCard.visible
                                && (mediaPreviewCard.isVideo
                                  || !mediaPreviewCard.downloaded)
                              opacity: mediaPreviewCard.isVideo
                                && mediaPreviewCard.downloaded
                                ? (mediaPreviewHover.hovered ? 1 : 0) : 1
                              width: Style.space(40)
                              height: Style.space(40)
                              iconSize: mediaPreviewCard.isVideo
                                && mediaPreviewCard.downloaded
                                ? Style.font.icon * 1.5 : Style.font.icon
                              iconText: downloading ? "󰔟"
                                : (mediaPreviewCard.isVideo
                                  && mediaPreviewCard.downloaded
                                  ? (mediaPreviewCard.inlinePlaying ? "󰏤" : "󰐊")
                                  : "󰇚")
                              tooltipText: downloading
                                ? "Downloading media"
                                : (mediaPreviewCard.isVideo
                                  ? (mediaPreviewCard.downloaded
                                    ? (mediaPreviewCard.inlinePlaying
                                      ? (mediaPreviewCard.isGif ? "Pause GIF" : "Pause video")
                                      : (mediaPreviewCard.isGif ? "Play GIF" : "Play video"))
                                    : (mediaPreviewCard.isGif ? "Download GIF" : "Download video"))
                                  : "Download full image")
                              foreground: root.foreground
                              accent: root.accent
                              enabled: visible && root.service && !downloading
                                && (!mediaPreviewCard.isVideo
                                  || !mediaPreviewCard.downloaded
                                  || mediaPreviewHover.hovered)

                              Behavior on opacity {
                                NumberAnimation {
                                  duration: 140
                                  easing.type: Easing.OutCubic
                                }
                              }

                              onClicked: {
                                if (mediaPreviewCard.isVideo
                                    && mediaPreviewCard.downloaded)
                                  root.toggleInlineVideo(mediaPreviewCard)
                                else
                                  root.service.downloadMedia(modelData)
                              }
                            }

                            Component.onDestruction:
                              root.stopInlineVideo(mediaPreviewCard)
                          }
                          Item {
                            id: voiceMessageCard
                            readonly property bool downloaded:
                              messageDelegate.mediaData
                              ? messageDelegate.mediaData.downloaded === true : false
                            readonly property string mediaPath:
                              messageDelegate.mediaData
                              ? String(messageDelegate.mediaData.path || "") : ""
                            readonly property bool active:
                              root.activeVoiceMessageCard === voiceMessageCard
                            readonly property bool playing: active
                              && voiceMessagePlayer.playbackState
                                === MediaPlayer.PlayingState
                            readonly property real totalSeconds: Math.max(0,
                              Number(messageDelegate.mediaData
                                ? messageDelegate.mediaData.duration_seconds || 0 : 0))
                            readonly property real elapsedSeconds: active
                              ? Math.max(0, Number(voiceMessagePlayer.position || 0) / 1000)
                              : 0
                            readonly property real progress: active
                              && voiceMessagePlayer.duration > 0
                              ? Math.min(1, voiceMessagePlayer.position
                                / voiceMessagePlayer.duration) : 0

                            visible: messageDelegate.mediaData
                              && messageDelegate.mediaData.kind === "audio"
                            width: parent.width
                            height: visible ? Style.space(48) : 0

                            CrispButton {
                              id: voiceMessageButton
                              readonly property bool downloading: visible
                                && root.service
                                && root.service.mediaDownloading(modelData)

                              anchors.left: parent.left
                              anchors.verticalCenter: parent.verticalCenter
                              width: Style.space(40)
                              height: Style.space(40)
                              iconText: downloading ? "󰔟"
                                : (voiceMessageCard.downloaded
                                  ? (voiceMessageCard.playing ? "󰏤" : "󰐊")
                                  : "󰇚")
                              tooltipText: downloading ? "Downloading voice message"
                                : (voiceMessageCard.downloaded
                                  ? (voiceMessageCard.playing
                                    ? "Pause voice message" : "Play voice message")
                                  : "Download voice message")
                              foreground: root.foreground
                              accent: root.accent
                              enabled: root.service && !downloading

                              onClicked: {
                                if (voiceMessageCard.downloaded)
                                  root.toggleVoiceMessage(voiceMessageCard)
                                else
                                  root.service.downloadMedia(modelData)
                              }
                            }

                            Column {
                              anchors.left: voiceMessageButton.right
                              anchors.right: parent.right
                              anchors.leftMargin: Style.space(10)
                              anchors.verticalCenter: parent.verticalCenter
                              spacing: Style.space(6)

                              Text {
                                width: parent.width
                                text: messageDelegate.mediaData
                                  && messageDelegate.mediaData.voice_message === false
                                  ? "Audio" : "Voice message"
                                color: root.foreground
                                font.family: root.fontFamily
                                font.pixelSize: Style.font.body
                                font.bold: true
                                elide: Text.ElideRight
                              }

                              Item {
                                width: parent.width
                                height: Math.max(voiceDuration.implicitHeight,
                                  Style.space(8))

                                Rectangle {
                                  id: voiceProgressTrack
                                  anchors.left: parent.left
                                  anchors.right: voiceDuration.left
                                  anchors.rightMargin: Style.space(10)
                                  anchors.verticalCenter: parent.verticalCenter
                                  height: Math.max(2, Style.normalBorderWidth)
                                  radius: height / 2
                                  color: Style.normalBorderFor(
                                    root.foreground, root.accent)

                                  Rectangle {
                                    width: parent.width * voiceMessageCard.progress
                                    height: parent.height
                                    radius: parent.radius
                                    color: root.accent
                                  }

                                  MouseArea {
                                    anchors.fill: parent
                                    enabled: voiceMessageCard.active
                                      && voiceMessagePlayer.duration > 0
                                    cursorShape: enabled
                                      ? Qt.PointingHandCursor : Qt.ArrowCursor
                                    onClicked: function(mouse) {
                                      voiceMessagePlayer.position = Math.round(
                                        mouse.x / width * voiceMessagePlayer.duration)
                                    }
                                  }
                                }

                                Text {
                                  id: voiceDuration
                                  anchors.right: parent.right
                                  anchors.verticalCenter: parent.verticalCenter
                                  text: Model.mediaDuration(voiceMessageCard.active
                                    ? voiceMessageCard.elapsedSeconds
                                    : voiceMessageCard.totalSeconds)
                                  color: root.timestamp
                                  font.family: root.fontFamily
                                  font.pixelSize: root.messageMetaFontSize
                                }
                              }
                            }

                            Component.onDestruction:
                              root.stopVoiceMessage(voiceMessageCard)
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
                          Rectangle {
                            id: locationCard
                            readonly property real liveUntil: {
                              if (!messageDelegate.mediaData
                                  || messageDelegate.mediaData.live !== true) return 0
                              var exact = Number(
                                messageDelegate.mediaData.live_until || 0)
                              if (exact > 0) return exact
                              var started = Number(
                                messageDelegate.mediaData.updated_at
                                || modelData.timestamp || 0)
                              var duration = Number(
                                messageDelegate.mediaData.duration_seconds || 0)
                              return started > 0 ? started + duration : 0
                            }
                            visible: messageDelegate.mediaData
                              && messageDelegate.mediaData.kind === "location"
                            width: parent.width
                            height: visible ? Style.space(120) : 0
                            radius: 0
                            clip: true
                            color: Style.normalFillFor(root.foreground, root.accent)

                            Image {
                              anchors.fill: parent
                              source: visible && root.service
                                && messageDelegate.mediaData.thumbnail_path
                                ? root.service.fileUrl(
                                  messageDelegate.mediaData.thumbnail_path)
                                  + "?revision=" + String(
                                    messageDelegate.mediaData.updated_at || 0) : ""
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
                                  color: Qt.rgba(root.background.r,
                                    root.background.g, root.background.b, 0.9)
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
                                width: parent.width - Style.space(7)
                                  - Style.font.icon
                                spacing: Style.space(1)

                                Text {
                                  width: parent.width
                                  text: String(messageDelegate.mediaData
                                    ? (messageDelegate.mediaData.name
                                      || messageDelegate.mediaData.address)
                                      || (messageDelegate.mediaData.live
                                        ? "Live location" : "Location")
                                    : "Location")
                                  color: root.foreground
                                  font.family: root.fontFamily
                                  font.pixelSize: Style.font.body
                                  font.bold: true
                                  elide: Text.ElideRight
                                }
                                Text {
                                  visible: messageDelegate.mediaData
                                    && String(messageDelegate.mediaData.name
                                      || "").length > 0
                                    && String(messageDelegate.mediaData.address
                                      || "").length > 0
                                  width: parent.width
                                  text: visible
                                    ? String(messageDelegate.mediaData.address) : ""
                                  color: root.muted
                                  font.family: root.fontFamily
                                  font.pixelSize: Style.font.caption
                                  elide: Text.ElideRight
                                }
                              }
                            }

                            Text {
                              id: liveRemainingLabel
                              visible: messageDelegate.mediaData
                                && messageDelegate.mediaData.live === true
                              anchors.right: parent.right
                              anchors.bottom: parent.bottom
                              anchors.margins: Style.space(10)
                              text: !visible ? ""
                                : locationCard.liveUntil > 0
                                  ? root.remainingTimeLabel(locationCard.liveUntil)
                                  : "Updated " + Model.messageTime(
                                    messageDelegate.mediaData.updated_at,
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
                              onClicked: if (root.service) root.service.openMap(
                                messageDelegate.mediaData.latitude_e7,
                                messageDelegate.mediaData.longitude_e7)
                            }

                            CrispButton {
                              anchors.centerIn: parent
                              visible: locationCard.visible
                              opacity: locationHover.hovered ? 1 : 0
                              width: Style.space(40)
                              height: Style.space(40)
                              iconSize: Style.font.icon * 1.5
                              iconText: "󰏌"
                              tooltipText: "Open map"
                              foreground: root.foreground
                              accent: root.accent
                              enabled: locationHover.hovered && root.service
                              z: 2

                              Behavior on opacity {
                                NumberAnimation {
                                  duration: 140
                                  easing.type: Easing.OutCubic
                                }
                              }

                              onClicked: root.service.openMap(
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
                        text: Model.messageTime(modelData.timestamp,
                          root.messageTimeFormat)
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

                CrispButton {
                  id: scrollToBottomButton
                  readonly property bool relevant: root.conversationReady
                    && messageList.count > 0
                    && messageList.visibleArea.yPosition
                      + messageList.visibleArea.heightRatio < 0.9999

                  anchors.horizontalCenter: parent.horizontalCenter
                  anchors.bottom: parent.bottom
                  anchors.bottomMargin: composerRow.height + Style.space(14)
                  z: 1
                  visible: relevant || opacity > 0
                  enabled: relevant
                  opacity: relevant ? 1 : 0
                  width: Style.space(30)
                  height: width
                  radius: width / 2
                  horizontalPadding: 0
                  verticalPadding: 0
                  iconText: "󰁅"
                  tooltipText: "Scroll to latest message"
                  foreground: root.foreground
                  background: root.background
                  accent: root.accent
                  bordered: true
                  focusable: true
                  onClicked: root.animateConversationViewportToBottom()

                  Behavior on opacity {
                    NumberAnimation {
                      duration: 140
                      easing.type: Easing.OutCubic
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
                color: root.sidebarSecondary
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
                color: root.sidebarSecondary
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
