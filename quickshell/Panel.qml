import QtQuick
import QtQuick.Controls as QQC
import QtQuick.Effects
import QtMultimedia
import QtQml.Models
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
  property string preservedConversationMessageId: ""
  property real preservedConversationMessageOffset: 0
  property string mediaDownloadAnchorChatJid: ""
  property string mediaDownloadAnchorMessageId: ""
  property real mediaDownloadAnchorOffset: 0
  property int mediaDownloadAnchorSerial: 0
  property string imagePreviewUrl: ""
  property var activeInlineVideoCard: null
  property bool activeInlineVideoGif: false
  property var activeVoiceMessageCard: null
  property bool voiceRecordingActive: false
  property string voiceRecordingId: ""
  property string voiceRecordingPath: ""
  property string voiceRecordingChatJid: ""
  property string voiceRecordingStopAction: ""
  property real currentTimestamp: Date.now() / 1000
  property var licenseEntries: []
  property string licenseLoadError: ""
  property alias appMenu: headerMenu
  property alias appMenuFirstAction: headerLicenseAction
  property alias chatStateResyncAction: headerResyncAction
  property alias chatStateResyncConfirmation: resyncConfirmation
  readonly property bool unreadOnly: service && service.unreadOnly === true
  readonly property bool voiceRecordingTestMode: service
    && service.voiceRecordingTestMode === true
  readonly property int voiceRecordingDurationMs: voiceRecordingTestMode
    ? Number(service.voiceRecordingTestDurationMs || 0)
    : Number(voiceRecorder.duration || 0)
  readonly property var voiceOutboxEntry: {
    var entries = service && Array.isArray(service.voiceOutboxEntries)
      ? service.voiceOutboxEntries : []
    var selected = service ? String(service.selectedChatJid || "") : ""
    for (var i = entries.length - 1; i >= 0; i--)
      if (String(entries[i].chat_jid || "") === selected) return entries[i]
    return null
  }
  readonly property var textOutboxEntry: service
    && typeof service.textOutboxForChat === "function"
    ? service.textOutboxForChat(service.selectedChatJid) : null

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
  readonly property bool daemonSetupRequired: service
    && service.daemonSetupRequired === true
  readonly property bool daemonSetupBusy: service
    && service.daemonSetupBusy === true
  readonly property string visibleError: service
    ? String(service.daemonSetupError || service.lastError || "") : ""
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
  readonly property var messageMentionContacts: {
    var output = []
    var chats = service ? service.chats : []
    if (!Array.isArray(chats)) chats = []
    for (var i = 0; i < chats.length; i++) {
      var chat = chats[i] || {}
      if (chat.is_group !== true) output.push(chat)
    }
    var participants = service && Array.isArray(service.groupParticipants)
      ? service.groupParticipants : []
    for (var j = 0; j < participants.length; j++)
      output.push(participants[j] || {})
    return output
  }

  onFilteredChatsChanged: root.syncChatRenderModel()

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

  component SquareControlButton: CrispButton {
    id: squareControlButton

    property real controlHeight: implicitHeight
    property string centeredIconText: ""
    property alias centeredIconItem: centeredIcon

    width: root.snapToDevicePixel(controlHeight)
    height: width
    iconText: ""

    Text {
      id: centeredIcon
      objectName: squareControlButton.objectName + "Icon"
      anchors.fill: parent
      horizontalAlignment: Text.AlignHCenter
      verticalAlignment: Text.AlignVCenter
      text: squareControlButton.centeredIconText
      color: squareControlButton.selected
        ? squareControlButton._selectedColor : squareControlButton.foreground
      font.family: squareControlButton.fontFamily
      font.pixelSize: squareControlButton.iconSize
    }
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

  function conversationChatActivity() {
    if (!service || !service.selectedChat) return ""
    var chat = service.selectedChat
    return typeof service.chatStateLabel === "function"
      ? service.chatStateLabel(chat.jid, chat.is_group === true) : ""
  }

  function conversationActivitySubtitle() {
    if (!service || !service.selectedChat) return ""
    var chat = service.selectedChat
    var activity = conversationChatActivity()
    if (activity) return activity
    if (chat.is_group === true) return groupConversationSubtitle()
    var presence = typeof service.presenceLabel === "function"
      ? service.presenceLabel(chat.jid, currentTimestamp,
        messageTimeFormat, Qt.locale()) : ""
    return presence || Model.contactPhoneNumber(chat.phone_number, chat.jid)
  }

  function sidebarChatActivity(chat) {
    if (!service || !chat || typeof service.chatStateLabel !== "function") return ""
    var activity = service.chatStateLabel(chat.jid, chat.is_group === true)
    if (!activity || chat.is_group === true) return String(activity || "")
    return activity.charAt(0).toUpperCase() + activity.slice(1)
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
    createPollPopup.close()
    imagePreviewPopup.close()
    stopInlineVideo()
    stopVoiceMessage()
    stopVoiceRecording(false)
    scrollToBottomAnimation.stop()
    opened = false
    controlHeld = false
    controlActivatorKey = 0
    if (service) service.setPanelState(false, false)
  }

  function openImagePreview(path, revision) {
    if (!service || !path) return
    imagePreviewUrl = service.fileUrl(path, revision)
    imagePreviewPopup.open()
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

  function mentionContactForJid(jid) {
    var value = String(jid || "")
    var phone = Model.contactPhoneNumber("", value).replace(/[^0-9]/g, "")
    if (!value || !phone) return null
    var contacts = messageMentionContacts
    for (var i = 0; i < contacts.length; i++) {
      var contact = contacts[i] || {}
      if (String(contact.jid || "") !== value) continue
      var resolved = Model.contactMention(phone, [contact])
      if (resolved) return resolved
    }
    return null
  }

  function ensureMentionDirectChat(contact) {
    return !!(service && contact && contact.jid
      && typeof service.ensureDirectChat === "function"
      && service.ensureDirectChat(contact.jid, contact.name))
  }

  function openMessageLink(link) {
    var value = String(link || "")
    var prefix = "mention:"
    if (value.indexOf(prefix) !== 0) {
      Qt.openUrlExternally(value)
      return true
    }
    var jid = ""
    try { jid = decodeURIComponent(value.substring(prefix.length)) }
    catch (error) { return false }
    var contact = mentionContactForJid(jid)
    if (!contact || !ensureMentionDirectChat(contact)) return false
    chooseChat(contact.jid)
    return true
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
    if (!jid || !service || typeof service.ensureDirectChat !== "function"
        || !service.ensureDirectChat(jid, "")) return
    newChat.text = ""
    newChatVisible = false
    service.selectChat(jid)
    Qt.callLater(function() { composer.forceActiveFocus() })
  }

  function submitMessage() {
    if (!service || !service.sendMessage(composer.text)) return
  }

  function startVoiceRecording() {
    if (voiceRecordingActive || !service
        || voiceOutboxEntry
        || typeof service.newVoiceRecording !== "function"
        || typeof service.beginVoiceRecording !== "function") return false
    var recording = service.newVoiceRecording()
    if (!recording || !recording.recording_id || !recording.path
        || !service.beginVoiceRecording()) return false
    voiceRecordingId = String(recording.recording_id)
    voiceRecordingPath = String(recording.path)
    voiceRecordingChatJid = String(recording.chat_jid || "")
    voiceRecordingStopAction = ""
    voiceRecordingActive = true
    if (!voiceRecordingTestMode) {
      voiceRecorder.outputLocation = "file://" + voiceRecordingPath
      voiceRecorder.record()
    }
    return true
  }

  function completeVoiceRecordingStop() {
    if (!voiceRecordingActive || !voiceRecordingStopAction) return false
    var action = voiceRecordingStopAction
    var recordingId = voiceRecordingId
    var chatJid = voiceRecordingChatJid
    var duration = Math.floor(voiceRecordingDurationMs)
    voiceRecordingActive = false
    voiceRecordingId = ""
    voiceRecordingPath = ""
    voiceRecordingChatJid = ""
    voiceRecordingStopAction = ""
    if (action === "send" && service
        && typeof service.sendVoiceMessage === "function") {
      var accepted = service.sendVoiceMessage(recordingId, chatJid, duration)
      if (accepted) scheduleConversationScroll("bottom", "")
      return accepted
    }
    if (service && typeof service.discardVoiceRecording === "function")
      service.discardVoiceRecording(recordingId)
    return action !== "send"
  }

  function stopVoiceRecording(sendRecording) {
    if (!voiceRecordingActive) return false
    voiceRecordingStopAction = sendRecording === true ? "send" : "discard"
    if (service && typeof service.finishVoiceRecording === "function")
      service.finishVoiceRecording()
    if (voiceRecordingTestMode
        || voiceRecorder.recorderState === MediaRecorder.StoppedState)
      return completeVoiceRecordingStop()
    voiceRecorder.stop()
    return true
  }

  function voiceRecordingFailed(message) {
    if (service)
      service.lastError = String(message || "Could not record voice message")
    return stopVoiceRecording(false)
  }

  function openPollCreator() {
    pollQuestion.text = ""
    pollOptions.text = ""
    createPollPopup.multipleAnswers = false
    createPollPopup.open()
  }

  function submitPoll() {
    if (!service) return
    var options = String(pollOptions.text || "").split(/\r?\n/)
    if (!service.createPoll(pollQuestion.text, options,
        createPollPopup.multipleAnswers)) return
    createPollPopup.close()
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

  function serializedMessage(message) {
    try { return JSON.stringify(message || {}) }
    catch (error) { return "{}" }
  }

  function messageRenderKey(message, index) {
    var id = String(message ? message.id || "" : "")
    return id ? "id:" + id : "index:" + index
  }

  function renderedMessageIndex(key, startIndex) {
    for (var i = Math.max(0, Number(startIndex || 0));
        i < conversationMessageModel.count; i++)
      if (conversationMessageModel.get(i).messageKey === key) return i
    return -1
  }

  function syncConversationMessageModel() {
    var desired = service && Array.isArray(service.messages)
      ? service.messages : []
    for (var index = 0; index < desired.length; index++) {
      var message = desired[index] || {}
      var key = messageRenderKey(message, index)
      var serialized = serializedMessage(message)
      var row = index < conversationMessageModel.count
        ? conversationMessageModel.get(index) : null
      if (!row || row.messageKey !== key) {
        var existingIndex = renderedMessageIndex(key, index + 1)
        if (existingIndex >= 0)
          conversationMessageModel.move(existingIndex, index, 1)
        else
          conversationMessageModel.insert(index, {
            messageKey: key,
            messageJson: serialized
          })
        row = conversationMessageModel.get(index)
      }
      if (row.messageJson !== serialized)
        conversationMessageModel.setProperty(index, "messageJson", serialized)
    }
    if (conversationMessageModel.count > desired.length)
      conversationMessageModel.remove(desired.length,
        conversationMessageModel.count - desired.length)
    if (messageList) messageList.forceLayout()
  }

  function chatRenderKey(chat, index) {
    var jid = String(chat ? chat.jid || "" : "")
    return jid ? "jid:" + jid : "index:" + index
  }

  function renderedChatIndex(key, startIndex) {
    for (var i = Math.max(0, Number(startIndex || 0));
        i < chatRenderModel.count; i++)
      if (chatRenderModel.get(i).chatKey === key) return i
    return -1
  }

  function syncChatRenderModel() {
    var desired = filteredChats
    for (var index = 0; index < desired.length; index++) {
      var chat = desired[index] || {}
      var key = chatRenderKey(chat, index)
      var serialized = serializedMessage(chat)
      var row = index < chatRenderModel.count
        ? chatRenderModel.get(index) : null
      if (!row || row.chatKey !== key) {
        var existingIndex = renderedChatIndex(key, index + 1)
        if (existingIndex >= 0)
          chatRenderModel.move(existingIndex, index, 1)
        else
          chatRenderModel.insert(index, {
            chatKey: key,
            chatJson: serialized
          })
        row = chatRenderModel.get(index)
      }
      if (row.chatJson !== serialized)
        chatRenderModel.setProperty(index, "chatJson", serialized)
    }
    if (chatRenderModel.count > desired.length)
      chatRenderModel.remove(desired.length,
        chatRenderModel.count - desired.length)
    if (chatList) chatList.forceLayout()
  }

  function clearMediaDownloadAnchor() {
    mediaDownloadAnchorClearTimer.stop()
    mediaDownloadAnchorSerial++
    mediaDownloadAnchorChatJid = ""
    mediaDownloadAnchorMessageId = ""
    mediaDownloadAnchorOffset = 0
  }

  function downloadMedia(message, delegateItem) {
    if (!service || !message || !delegateItem || !messageList) return false
    messageList.forceLayout()
    mediaDownloadAnchorChatJid = String(message.chat_jid || "")
    mediaDownloadAnchorMessageId = String(message.id || "")
    mediaDownloadAnchorOffset = delegateItem.y - messageList.contentY
    mediaDownloadAnchorSerial++
    mediaDownloadAnchorClearTimer.stop()
    if (service.downloadMedia(message)) return true
    clearMediaDownloadAnchor()
    return false
  }

  function restoreMediaDownloadAnchor(serial, remainingPasses) {
    if (serial !== mediaDownloadAnchorSerial || !messageList
        || !mediaDownloadAnchorMessageId) return
    var index = messageIndex(mediaDownloadAnchorMessageId)
    if (index < 0) return
    messageList.forceLayout()
    var delegateItem = messageList.itemAtIndex(index)
    if (!delegateItem) {
      messageList.positionViewAtIndex(index, ListView.Beginning)
      messageList.forceLayout()
      delegateItem = messageList.itemAtIndex(index)
    }
    if (delegateItem)
      messageList.contentY = delegateItem.y - mediaDownloadAnchorOffset
    if (remainingPasses > 0) {
      Qt.callLater(function() {
        root.restoreMediaDownloadAnchor(serial, remainingPasses - 1)
      })
    } else {
      mediaDownloadAnchorClearTimer.restart()
    }
  }

  function scheduleMediaDownloadAnchorRestore(messageId) {
    if (!mediaDownloadAnchorMessageId
        || (messageId && String(messageId) !== mediaDownloadAnchorMessageId)) return
    var serial = ++mediaDownloadAnchorSerial
    Qt.callLater(function() {
      root.restoreMediaDownloadAnchor(serial, 2)
    })
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
      var target = root
      Qt.callLater(function() {
        if (target && typeof target.alignConversationViewportToBottom === "function")
          target.alignConversationViewportToBottom(serial, nextPass)
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

  function preserveConversationPosition() {
    if (!messageList || !messageList.count) return false
    messageList.forceLayout()
    preservedConversationContentY = messageList.contentY
    var index = messageList.indexAt(1, messageList.contentY + 1)
    if (index < 0) index = messageList.indexAt(1,
      messageList.contentY + Math.max(1, messageList.spacing + 1))
    if (index < 0 || !service || index >= service.messages.length) {
      preservedConversationMessageId = ""
      preservedConversationMessageOffset = 0
    } else {
      var item = messageList.itemAtIndex(index)
      preservedConversationMessageId = String(
        (service.messages[index] || {}).id || "")
      preservedConversationMessageOffset = item
        ? item.y - messageList.contentY : 0
    }
    restoreConversationAfterMessages = true
    return true
  }

  function restoreConversationPosition(serial, remainingPasses) {
    if (serial !== conversationScrollSerial
        || !restoreConversationAfterMessages || !messageList) return
    messageList.forceLayout()
    var index = messageIndex(preservedConversationMessageId)
    var item = index >= 0 ? messageList.itemAtIndex(index) : null
    if (index >= 0 && !item) {
      messageList.positionViewAtIndex(index, ListView.Beginning)
      messageList.forceLayout()
      item = messageList.itemAtIndex(index)
    }
    if (item)
      messageList.contentY = item.y - preservedConversationMessageOffset
    else
      messageList.contentY = preservedConversationContentY
    if (remainingPasses > 0) {
      Qt.callLater(function() {
        root.restoreConversationPosition(serial, remainingPasses - 1)
      })
    } else {
      restoreConversationAfterMessages = false
      preservedConversationMessageId = ""
    }
  }

  function scheduleConversationPositionRestore() {
    var serial = ++conversationScrollSerial
    Qt.callLater(function() {
      root.restoreConversationPosition(serial, 2)
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

  function messageReceiptIcon(receipt) {
    var value = Math.max(0, Math.min(4, Math.floor(Number(receipt || 0))))
    if (value === 0) return "󰥔"
    if (value === 1) return "✓"
    if (value < 4) return "✓✓"
    return "󰍬"
  }

  function messageReceiptLabel(receipt) {
    var labels = ["Waiting to send", "Sent", "Delivered", "Read", "Played"]
    var value = Math.max(0, Math.min(4, Math.floor(Number(receipt || 0))))
    return labels[value]
  }

  function messageReceiptTimestamp(timestamp) {
    var value = Math.floor(Number(timestamp || 0))
    return value > 0 ? Model.messageTime(value, root.messageTimeFormat) : ""
  }

  function messageReceiptGroups(message) {
    var value = message || {}
    var readJids = ({})
    var readEntries = []
    var readers = value.read_by
    if (readers && typeof readers.length === "number") {
      for (var readerIndex = 0; readerIndex < readers.length; readerIndex++) {
        var reader = readers[readerIndex] || {}
        var readerJid = String(reader.jid || "")
        if (!readerJid || readJids[readerJid] === true) continue
        readJids[readerJid] = true
        var readerName = Model.friendlyName(reader.name, readerJid)
        var readerTime = root.messageReceiptTimestamp(reader.read_at)
        readEntries.push({
          name: readerName,
          text: readerName + (readerTime ? " · " + readerTime : "")
        })
      }
    }
    readEntries.sort(function (left, right) {
      return left.name.localeCompare(right.name)
    })

    var deliveredJids = ({})
    var deliveredEntries = []
    var deliveries = value.delivered_to
    if (deliveries && typeof deliveries.length === "number") {
      for (var deliveryIndex = 0;
          deliveryIndex < deliveries.length; deliveryIndex++) {
        var delivery = deliveries[deliveryIndex] || {}
        var deliveryJid = String(delivery.jid || "")
        if (!deliveryJid || readJids[deliveryJid] === true
            || deliveredJids[deliveryJid] === true) continue
        deliveredJids[deliveryJid] = true
        var deliveryName = Model.friendlyName(delivery.name, deliveryJid)
        var deliveryTime = root.messageReceiptTimestamp(delivery.delivered_at)
        deliveredEntries.push({
          name: deliveryName,
          text: deliveryName + (deliveryTime ? " · " + deliveryTime : "")
        })
      }
    }
    deliveredEntries.sort(function (left, right) {
      return left.name.localeCompare(right.name)
    })

    var groups = []
    if (readEntries.length) groups.push({
      label: Number(value.receipt || 0) >= 4 ? "Played" : "Read",
      entries: readEntries.map(function (entry) { return entry.text })
    })
    if (deliveredEntries.length) groups.push({
      label: "Delivered",
      entries: deliveredEntries.map(function (entry) { return entry.text })
    })
    if (!groups.length) groups.push({
      label: root.messageReceiptLabel(value.receipt),
      entries: []
    })
    return groups
  }

  function messageReceiptTooltip(message) {
    var groups = root.messageReceiptGroups(message)
    var sections = []
    for (var groupIndex = 0; groupIndex < groups.length; groupIndex++) {
      var group = groups[groupIndex]
      sections.push([group.label].concat(group.entries).join("\n"))
    }
    return sections.join("\n\n")
  }

  Timer {
    interval: 30000
    repeat: true
    running: root.opened
    triggeredOnStart: true
    onTriggered: root.currentTimestamp = Date.now() / 1000
  }

  ListModel {
    id: conversationMessageModel
    objectName: "conversationMessageModel"
  }

  ListModel {
    id: chatRenderModel
    objectName: "chatRenderModel"
  }

  Component.onCompleted: {
    root.syncConversationMessageModel()
    root.syncChatRenderModel()
  }

  Timer {
    id: mediaDownloadAnchorClearTimer
    interval: 750
    repeat: false
    onTriggered: root.clearMediaDownloadAnchor()
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

  AudioInput {
    id: voiceRecordingInput
  }

  MediaRecorder {
    id: voiceRecorder
    mediaFormat.fileFormat: MediaFormat.Ogg
    mediaFormat.audioCodec: MediaFormat.AudioCodec.Opus
    quality: MediaRecorder.NormalQuality
    encodingMode: MediaRecorder.ConstantBitRateEncoding
    audioBitRate: 32000
    audioChannelCount: 1
    audioSampleRate: 48000

    onRecorderStateChanged: {
      if (recorderState === MediaRecorder.StoppedState
          && root.voiceRecordingActive && root.voiceRecordingStopAction)
        root.completeVoiceRecordingStop()
    }
    onDurationChanged: {
      if (duration >= 15 * 60 * 1000 && root.voiceRecordingActive)
        root.stopVoiceRecording(true)
    }
    onErrorOccurred: function(error, errorString) {
      if (error !== MediaRecorder.NoError)
        root.voiceRecordingFailed(errorString)
    }
  }

  CaptureSession {
    audioInput: voiceRecordingInput
    recorder: voiceRecorder
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
    function onTextMessageAccepted(_deliveryId, chatJid, text) {
      if (root.service && root.service.selectedChatJid === String(chatJid || "")
          && composer.text === String(text || "")) composer.text = ""
      root.scheduleConversationScroll("bottom", "")
    }
    function onSelectedChatJidChanged() {
      root.stopVoiceRecording(false)
      imagePreviewPopup.close()
      root.clearMediaDownloadAnchor()
      root.stopInlineVideo()
      root.stopVoiceMessage()
      scrollToBottomAnimation.stop()
      root.conversationScrollSerial++
      root.conversationReady = false
      root.scrollToBottomAfterMessages = false
      root.restoreConversationAfterMessages = false
      root.preservedConversationMessageId = ""
      Qt.callLater(root.revealSelectedChat)
    }
    function onConnectionStateChanged() {
      if (!root.service || root.service.connectionState !== "connected")
        root.stopVoiceRecording(false)
    }
    function onMessagesWillChange(preservePosition) {
      root.stopInlineVideo()
      root.stopVoiceMessage()
      if (root.mediaDownloadAnchorMessageId) return
      if (!preservePosition || !messageList || !root.conversationReady
          || root.restoreConversationAfterMessages) return
      root.preserveConversationPosition()
    }
    function onMessagesChanged() {
      root.syncConversationMessageModel()
      if (root.restoreConversationAfterMessages
          && !root.mediaDownloadAnchorMessageId)
        root.scheduleConversationPositionRestore()
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
      } else if (root.mediaDownloadAnchorMessageId) {
        root.scheduleMediaDownloadAnchorRestore("")
      } else if (root.restoreConversationAfterMessages) {
        root.scheduleConversationPositionRestore()
      }
    }
    function onMediaDownloadRequestsChanged() {
      if (!root.mediaDownloadAnchorMessageId || !root.service) return
      var key = root.mediaDownloadAnchorChatJid + "\n"
        + root.mediaDownloadAnchorMessageId
      if (root.service.mediaDownloadRequests[key] !== true)
        mediaDownloadAnchorClearTimer.restart()
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
        if (resyncConfirmation.handleKey(event)) {
          event.accepted = true
          return
        }
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
        id: resyncConfirmation

        anchors.fill: parent
        z: 1000
        message: "Resync chat state from WhatsApp?\n\nThis replays unread, "
          + "pinned, archived, and muted state from WhatsApp. Your linked account, "
          + "messages, media, and local history stay intact."
        confirmText: "Resync"
        background: Color.popups.background
        foreground: Color.popups.text
        selectedText: root.accent

        onOpenedChanged: if (opened) selectedIndex = 0
        onCanceled: opened = false
        onConfirmed: {
          if (root.service && root.service.requestChatStateResync()) opened = false
        }
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
        id: imagePreviewPopup

        parent: focusScope
        x: 0
        y: 0
        width: parent.width
        height: parent.height
        padding: 0
        modal: true
        focus: true
        closePolicy: QQC.Popup.CloseOnEscape

        onClosed: root.imagePreviewUrl = ""

        background: Rectangle {
          color: Qt.rgba(root.background.r, root.background.g,
            root.background.b, 0.96)
        }

        contentItem: Item {
          MouseArea {
            anchors.fill: parent
            onClicked: imagePreviewPopup.close()
          }

          Image {
            id: fullImagePreview

            anchors.fill: parent
            anchors.margins: Style.space(28)
            source: root.imagePreviewUrl
            asynchronous: true
            cache: false
            fillMode: Image.PreserveAspectFit
            smooth: true
            mipmap: true
          }

          MouseArea {
            x: fullImagePreview.x
              + (fullImagePreview.width - fullImagePreview.paintedWidth) / 2
            y: fullImagePreview.y
              + (fullImagePreview.height - fullImagePreview.paintedHeight) / 2
            width: fullImagePreview.paintedWidth
            height: fullImagePreview.paintedHeight
            enabled: fullImagePreview.status === Image.Ready
            onClicked: function(mouse) { mouse.accepted = true }
          }

          Text {
            anchors.centerIn: parent
            visible: fullImagePreview.status === Image.Loading
              || fullImagePreview.status === Image.Error
            text: fullImagePreview.status === Image.Error
              ? "Image unavailable" : "Loading image…"
            color: root.foreground
            font.family: root.fontFamily
            font.pixelSize: Style.font.body
          }

          CrispButton {
            anchors.top: parent.top
            anchors.right: parent.right
            anchors.margins: Style.space(16)
            width: Style.space(40)
            height: Style.space(40)
            iconSize: Style.font.icon * 1.5
            iconText: "󰅖"
            foreground: root.foreground
            accent: root.accent
            tooltipText: "Close image preview"
            focusable: true
            onClicked: imagePreviewPopup.close()
          }
        }
      }

      QQC.Popup {
        id: createPollPopup
        objectName: "createPollPopup"

        property bool multipleAnswers: false
        readonly property var normalizedOptions: {
          var values = String(pollOptions.text || "").split(/\r?\n/)
          var output = []
          for (var i = 0; i < values.length; i++) {
            var value = String(values[i] || "").trim()
            if (value && output.indexOf(value) < 0) output.push(value)
          }
          return output
        }
        readonly property bool valid: String(pollQuestion.text || "").trim() !== ""
          && normalizedOptions.length >= 2 && normalizedOptions.length <= 12
          && root.service && root.service.pollCreateRequestId === 0
        readonly property var popupBorderSpec:
          Border.localOrSurfaceSpec("popups", "border",
            Color.popups.border, Color.popups.border,
            Math.max(1, Style.normalBorderWidth))

        parent: focusScope
        x: Math.round((parent.width - width) / 2)
        y: Math.round((parent.height - height) / 2)
        width: Math.min(Style.space(520), parent.width - Style.space(48))
        height: Math.min(Style.space(470), parent.height - Style.space(48))
        padding: Style.space(18)
        modal: true
        focus: true
        closePolicy: QQC.Popup.CloseOnEscape | QQC.Popup.CloseOnPressOutside

        onOpened: Qt.callLater(function() { pollQuestion.forceActiveFocus() })

        background: CrispBorderSurface {
          color: Color.popups.background
          sourceBorderSpec: createPollPopup.popupBorderSpec
          radius: Style.cornerRadius + Style.space(4)
        }

        contentItem: Column {
          spacing: Style.space(10)

          Text {
            width: parent.width
            text: "Create poll"
            color: Color.popups.text
            font.family: root.fontFamily
            font.pixelSize: Style.font.title
            font.bold: true
          }

          CrispTextField {
            id: pollQuestion
            objectName: "pollQuestion"
            width: parent.width
            placeholderText: "Question"
          }

          Text {
            width: parent.width
            text: "Options — one per line (2–12)"
            color: Color.popups.text
            font.family: root.fontFamily
            font.pixelSize: Style.font.caption
          }

          QQC.TextArea {
            id: pollOptions
            objectName: "pollOptions"
            width: parent.width
            height: Style.space(220)
            color: Color.popups.text
            placeholderText: "First option\nSecond option"
            placeholderTextColor: Qt.rgba(Color.popups.text.r,
              Color.popups.text.g, Color.popups.text.b, 0.55)
            selectionColor: root.accent
            selectedTextColor: Color.popups.background
            font.family: root.fontFamily
            font.pixelSize: Style.font.body
            wrapMode: TextEdit.Wrap
            padding: Style.space(10)
            background: CrispBorderSurface {
              color: Style.normalFillFor(Color.popups.text, root.accent)
              sourceBorderSpec: Border.flat(
                Style.normalBorderFor(Color.popups.text, root.accent),
                Math.max(1, Style.normalBorderWidth))
              radius: Style.cornerRadius
            }
          }

          Row {
            width: parent.width
            spacing: Style.space(8)

            CrispButton {
              id: multipleAnswersButton
              text: createPollPopup.multipleAnswers
                ? "✓ Multiple answers" : "Multiple answers"
              bordered: true
              selected: createPollPopup.multipleAnswers
              foreground: Color.popups.text
              onClicked: createPollPopup.multipleAnswers
                = !createPollPopup.multipleAnswers
            }

            Item {
              width: parent.width - multipleAnswersButton.width - createPollButton.width
                - cancelPollButton.width - parent.spacing * 3
              height: 1
            }

            CrispButton {
              id: cancelPollButton
              text: "Cancel"
              bordered: true
              foreground: Color.popups.text
              onClicked: createPollPopup.close()
            }

            CrispButton {
              id: createPollButton
              objectName: "createPollButton"
              text: "Create"
              iconText: "󰘻"
              bordered: true
              active: createPollPopup.valid
              enabled: createPollPopup.valid
              foreground: Color.popups.text
              onClicked: root.submitPoll()
            }
          }
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

              objectName: "headerMoreButton"
              iconText: "󰇙"
              foreground: root.foreground
              tooltipText: "More"
              focusable: true
              selected: headerMenu.opened
              onClicked: headerMenu.opened ? headerMenu.close() : headerMenu.open()

              QQC.Popup {
                id: headerMenu

                objectName: "headerMenu"
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

                    objectName: "headerLicenseAction"
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
                    id: headerResyncAction

                    objectName: "headerResyncAction"
                    width: parent.width
                    visible: root.paired
                    enabled: root.service
                      && root.service.chatStateResyncBusy !== true
                    menuIconText: "󰑐"
                    menuText: root.service
                      && root.service.chatStateResyncBusy === true
                      ? "Resyncing chat state…" : "Resync chat state"
                    foreground: Color.popups.text
                    accent: root.accent
                    focusable: true
                    onClicked: {
                      headerMenu.close()
                      Qt.callLater(function() {
                        resyncConfirmation.opened = true
                      })
                    }
                  }

                  Text {
                    objectName: "headerResyncStatus"
                    width: parent.width
                    visible: root.service
                      && root.service.chatStateResyncStatus !== "idle"
                    text: root.service
                      ? String(root.service.chatStateResyncMessage || "") : ""
                    color: root.service
                      && root.service.chatStateResyncStatus === "failed"
                      ? Color.urgent : (root.service
                        && root.service.chatStateResyncStatus === "succeeded"
                        ? root.accent : root.muted)
                    font.family: root.fontFamily
                    font.pixelSize: Style.font.caption
                    wrapMode: Text.Wrap
                    leftPadding: Style.space(8)
                    rightPadding: Style.space(8)
                    topPadding: Style.space(2)
                    bottomPadding: Style.space(4)
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
                      id: sidebarFilterRow

                      readonly property real controlHeight:
                        root.snapToDevicePixel(chatSearch.implicitHeight)

                      width: parent.width - parent.padding * 2
                      spacing: root.snapToDevicePixel(Style.space(6))
                      CrispTextField {
                        id: chatSearch
                        objectName: "chatSearch"
                        height: sidebarFilterRow.controlHeight
                        width: parent.width - unreadFilterButton.width
                          - newChatButton.width - parent.spacing * 2
                        placeholderText: "Search conversations"
                        onAccepted: if (root.filteredChats.length)
                          root.chooseChat(root.filteredChats[0].jid)
                      }
                      SquareControlButton {
                        id: unreadFilterButton
                        objectName: "unreadFilterButton"
                        controlHeight: sidebarFilterRow.controlHeight
                        centeredIconText: "󰈲"
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
                      SquareControlButton {
                        id: newChatButton
                        objectName: "newChatButton"
                        controlHeight: sidebarFilterRow.controlHeight
                        centeredIconText: root.newChatVisible ? "󰅖" : "󰐕"
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
                        objectName: "newChat"
                        width: parent.width - openChatButton.width - parent.spacing
                        placeholderText: "Phone number or JID"
                        onAccepted: root.openNewChat()
                      }
                      CrispButton {
                        id: openChatButton
                        objectName: "openChatButton"
                        text: "Open"
                        bordered: true
                        foreground: root.foreground
                        onClicked: root.openNewChat()
                      }
                    }
                  }

                  ListView {
                    id: chatList
                    objectName: "chatList"
                    width: parent.width
                    height: parent.height - sidebarTools.height
                    clip: true
                    model: chatRenderModel
                    boundsBehavior: Flickable.StopAtBounds
                    reuseItems: true
                    QQC.ScrollBar.vertical: QQC.ScrollBar {}

                    onDraggingChanged: {
                      if (dragging) root.clearMediaDownloadAnchor()
                    }

                    delegate: Item {
                      id: chatDelegate

                      required property string chatJson
                      required property int index
                      readonly property var modelData: {
                        try { return JSON.parse(chatJson || "{}") || {} }
                        catch (error) { return ({}) }
                      }
                      objectName: "chatRow-" + String(modelData.jid || "")
                      property alias contextMenu: chatContextMenu
                      property alias pinAction: chatPinAction
                      width: chatList.width
                      height: Style.space(60)
                      readonly property bool selected: root.service
                        && String(modelData.jid || "") === root.service.selectedChatJid
                      readonly property string activityText:
                        root.sidebarChatActivity(modelData)

                      function openContextMenuAt(pointerX, pointerY) {
                        chatContextMenu.x = Math.max(0,
                          Math.min(pointerX, chatDelegate.width
                            - chatContextMenu.width))
                        chatContextMenu.y = pointerY
                        chatContextMenu.open()
                      }

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
                              anchors.right: timeLabel.left
                              anchors.rightMargin: Style.space(5)
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
                              objectName: "chatPreview-" + String(modelData.jid || "")
                              anchors.left: parent.left
                              anchors.right: chatStatusIcons.left
                              anchors.rightMargin: chatStatusIcons.visible
                                ? Style.space(5) : 0
                              text: {
                                if (chatDelegate.activityText)
                                  return chatDelegate.activityText
                                var message = String(modelData.last_message || "")
                                if (!message) return "No messages yet"
                                var sender = String(modelData.last_sender_name || "")
                                return modelData.is_group === true && sender
                                  ? sender + ": " + message : message
                              }
                              color: chatDelegate.activityText
                                ? root.accent : root.sidebarSecondary
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
                                || Number(modelData.unread || 0) > 0
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
                              Rectangle {
                                id: unreadBadge
                                visible: Number(modelData.unread || 0) > 0
                                width: visible ? Math.max(Style.space(14),
                                  unreadText.implicitWidth + Style.space(4)) : 0
                                height: visible ? Style.space(14) : 0
                                radius: height / 2
                                color: root.accent
                                Text {
                                  id: unreadText
                                  anchors.centerIn: parent
                                  text: Number(modelData.unread || 0) > 99
                                    ? "99+" : String(modelData.unread || 0)
                                  color: root.muted
                                  font.family: root.fontFamily
                                  font.pixelSize: Style.font.caption
                                  font.bold: true
                                }
                              }
                            }
                          }
                        }
                      }

                      MouseArea {
                        id: rowMouse
                        anchors.fill: parent
                        hoverEnabled: true
                        acceptedButtons: Qt.LeftButton | Qt.RightButton
                        cursorShape: Qt.PointingHandCursor
                        onClicked: function(mouse) {
                          if (mouse.button === Qt.RightButton) {
                            chatDelegate.openContextMenuAt(mouse.x, mouse.y)
                          } else {
                            root.chooseChat(modelData.jid)
                          }
                        }
                      }

                      QQC.Popup {
                        id: chatContextMenu

                        objectName: "chatContextMenu-"
                          + String(modelData.jid || "")
                        readonly property var popupBorderSpec:
                          Border.localOrSurfaceSpec("popups", "border",
                            Color.popups.border, Color.popups.border,
                            Math.max(1, Style.normalBorderWidth))

                        parent: chatDelegate
                        width: Style.space(188)
                        height: chatPinAction.implicitHeight
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

                        background: CrispBorderSurface {
                          color: Color.popups.background
                          sourceBorderSpec: chatContextMenu.popupBorderSpec
                          radius: Style.cornerRadius + Style.space(2)
                        }

                        contentItem: CrispMenuButton {
                          id: chatPinAction

                          objectName: "chatPinAction-"
                            + String(modelData.jid || "")
                          width: parent.width
                          menuIconText: "󰐃"
                          menuText: modelData.pinned === true
                            ? "Unpin conversation" : "Pin conversation"
                          foreground: Color.popups.text
                          accent: root.accent
                          focusable: true
                          onClicked: {
                            chatContextMenu.close()
                            if (root.service
                                && typeof root.service.setChatPinned === "function")
                              root.service.setChatPinned(modelData.jid,
                                modelData.pinned !== true)
                          }
                        }
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
                          objectName: "conversationTitle"
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
                          objectName: "conversationSubtitle"
                          readonly property bool showingChatActivity:
                            root.conversationChatActivity() !== ""
                          visible: root.service && root.service.selectedChat
                            && conversationSubtitle.text !== ""
                          text: root.conversationActivitySubtitle()
                          color: showingChatActivity
                            ? root.accent : root.foreground
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
                    objectName: "messageList"
                    width: parent.width
                    height: parent.height - conversationHeader.height - composerRow.height
                    clip: true
                    opacity: root.conversationReady
                      || !(root.service && root.service.selectedChatJid) ? 1 : 0
                    interactive: root.conversationReady
                      || !(root.service && root.service.selectedChatJid)
                    model: conversationMessageModel
                    spacing: Style.space(4)
                    topMargin: Style.space(12)
                    bottomMargin: Style.space(12)
                    boundsBehavior: Flickable.StopAtBounds
                    reuseItems: true
                    QQC.ScrollBar.vertical: QQC.ScrollBar {}

                    delegate: Item {
                      id: messageDelegate
                      objectName: "messageDelegate-" + String(modelData.id || "")
                      required property string messageJson
                      required property int index
                      readonly property var modelData: {
                        try { return JSON.parse(messageJson || "{}") || {} }
                        catch (error) { return ({}) }
                      }
                      readonly property var mediaData: root.service
                        ? root.service.messageMedia(modelData)
                        : modelData.media || null
                      readonly property var reactionsData: modelData.reactions || []
                      readonly property int reactionCount: reactionsData
                        && typeof reactionsData.length === "number"
                        ? reactionsData.length : 0
                      readonly property bool isPoll: mediaData
                        && mediaData.kind === "poll"
                      readonly property bool isSticker: mediaData
                        && mediaData.kind === "sticker"
                      readonly property bool hasStructuredMedia: mediaData
                        && (mediaData.kind === "image"
                          || mediaData.kind === "sticker"
                          || mediaData.kind === "video"
                          || mediaData.kind === "audio"
                          || mediaData.kind === "document"
                          || mediaData.kind === "location"
                          || mediaData.kind === "poll")
                      readonly property bool hasMediaCaption: mediaData
                        && String(modelData.text || "").charAt(0) !== "["
                      readonly property bool showSenderLabel: root.service
                        && root.service.selectedChat
                        && root.service.selectedChat.is_group === true
                      readonly property string senderLabelText: modelData.from_me
                        ? "Me" : Model.friendlyName(modelData.sender_name,
                          modelData.sender_jid)
                      readonly property string renderedMessageText:
                        Model.linkifiedMessage(modelData.text, root.accent,
                          root.messageMentionContacts)
                      readonly property bool showSenderAvatar: !modelData.from_me
                        && showSenderLabel
                      readonly property var previousMessage: root.service
                        && index > 0 && index <= root.service.messages.length
                        ? root.service.messages[index - 1] : null
                      readonly property var nextMessage: root.service
                        && index + 1 < root.service.messages.length
                        ? root.service.messages[index + 1] : null
                      readonly property string messageDateKey:
                        Model.messageDateKey(modelData.timestamp)
                      readonly property bool showDateDivider: messageDateKey !== ""
                        && (!previousMessage || messageDateKey
                          !== Model.messageDateKey(previousMessage.timestamp))
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

                      function selectedPollOptions() {
                        var selected = []
                        if (!isPoll || !Array.isArray(mediaData.options)) return selected
                        for (var i = 0; i < mediaData.options.length; i++)
                          if (mediaData.options[i].selected_by_me === true)
                            selected.push(String(mediaData.options[i].name || ""))
                        return selected
                      }

                      function togglePollOption(optionIndex) {
                        if (!root.service || !isPoll || pollCard.ended
                            || root.service.pollVotePending(modelData)) return
                        var options = mediaData.options || []
                        var option = options[optionIndex] || {}
                        var name = String(option.name || "")
                        if (!name) return
                        var selected = selectedPollOptions()
                        var selectedIndex = selected.indexOf(name)
                        if (Number(mediaData.selectable_count || 1) === 1) {
                          selected = selectedIndex >= 0 ? [] : [name]
                        } else if (selectedIndex >= 0) {
                          selected.splice(selectedIndex, 1)
                        } else {
                          selected.push(name)
                        }
                        root.service.votePoll(modelData, selected)
                      }

                      width: messageList.width
                      height: dateDivider.height
                        + Math.max(bubble.height, senderAvatar.height)
                        + (reactionsBar.visible
                          ? reactionsBar.height - Style.space(6) : 0)
                        + (messageFooter.visible
                          ? messageFooter.implicitHeight + Style.space(3) : 0)
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

                      Item {
                        id: dateDivider
                        objectName: "dateDivider-" + String(modelData.id || "")
                        readonly property real lineWidth: Math.max(0,
                          width - dateDividerLabelBackground.width
                            - Style.space(48)) / 2
                        readonly property real contentHeight: Style.space(34)
                        readonly property real bottomSpacing: Style.space(12)
                        readonly property real contentCenterOffset:
                          -bottomSpacing / 2
                        visible: messageDelegate.showDateDivider
                        width: parent.width
                        height: messageDelegate.showDateDivider
                          ? contentHeight + bottomSpacing : 0

                        Rectangle {
                          objectName: "dateDividerLeftLine-"
                            + String(modelData.id || "")
                          anchors.left: parent.left
                          anchors.leftMargin: Style.space(16)
                          anchors.verticalCenter: parent.verticalCenter
                          anchors.verticalCenterOffset:
                            dateDivider.contentCenterOffset
                          width: dateDivider.lineWidth
                          height: Math.max(1, Style.normalBorderWidth)
                          color: Qt.rgba(root.foreground.r, root.foreground.g,
                            root.foreground.b, root.foreground.a * 0.28)
                        }

                        Rectangle {
                          objectName: "dateDividerRightLine-"
                            + String(modelData.id || "")
                          anchors.right: parent.right
                          anchors.rightMargin: Style.space(16)
                          anchors.verticalCenter: parent.verticalCenter
                          anchors.verticalCenterOffset:
                            dateDivider.contentCenterOffset
                          width: dateDivider.lineWidth
                          height: Math.max(1, Style.normalBorderWidth)
                          color: Qt.rgba(root.foreground.r, root.foreground.g,
                            root.foreground.b, root.foreground.a * 0.28)
                        }

                        Rectangle {
                          id: dateDividerLabelBackground
                          anchors.centerIn: parent
                          anchors.verticalCenterOffset:
                            dateDivider.contentCenterOffset
                          width: dateDividerLabel.implicitWidth + Style.space(12)
                          height: dateDividerLabel.implicitHeight + Style.space(4)
                          color: root.background

                          Text {
                            id: dateDividerLabel
                            objectName: "dateDividerLabel-"
                              + String(modelData.id || "")
                            anchors.centerIn: parent
                            text: Model.messageDateLabel(modelData.timestamp,
                              Qt.locale())
                            color: root.timestamp
                            font.family: root.fontFamily
                            font.pixelSize: root.messageMetaFontSize + 2
                            font.bold: true
                          }
                        }
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
                        objectName: "messageBubble-" + String(modelData.id || "")
                        readonly property bool stickerOnlyMedia:
                          messageDelegate.isSticker
                        readonly property bool borderOnlyMedia:
                          messageDelegate.mediaData
                          && (messageDelegate.mediaData.kind === "video"
                            || messageDelegate.mediaData.kind === "location")
                        readonly property color mediaBorderColor: {
                          var base = Style.normalBorderFor(
                            root.foreground, root.accent)
                          return Qt.rgba(base.r, base.g, base.b, base.a * 0.55)
                        }
                        readonly property real horizontalPadding: borderOnlyMedia
                          ? borderLeft + borderRight
                          : (stickerOnlyMedia ? 0 : Style.space(22))
                        readonly property real maximumWidth: messageList.width * 0.72
                        readonly property real maximumMediaWidth:
                          Math.min(maximumWidth, Style.space(340))
                            - horizontalPadding
                        readonly property real locationPreviewWidth:
                          Math.min(maximumMediaWidth, Style.space(260))
                        readonly property real mediaAspectRatio:
                          Number(messageDelegate.mediaData
                            ? messageDelegate.mediaData.width || 1 : 1)
                            / Math.max(1, Number(messageDelegate.mediaData
                              ? messageDelegate.mediaData.height || 1 : 1))
                        readonly property real imageAspectRatio:
                          mediaPreviewImage.status === Image.Ready
                            && mediaPreviewImage.sourceSize.width > 0
                            && mediaPreviewImage.sourceSize.height > 0
                          ? mediaPreviewImage.sourceSize.width
                            / mediaPreviewImage.sourceSize.height
                          : mediaAspectRatio
                        readonly property real imagePreviewWidth: Math.max(
                          Style.space(40), Math.min(maximumMediaWidth,
                            Style.space(280) * imageAspectRatio))
                        readonly property real videoPreviewWidth: Math.max(
                          Style.space(40), Math.min(maximumMediaWidth,
                            Style.space(280) * mediaAspectRatio))
                        readonly property real stickerAspectRatio: Math.max(
                          0.5, Math.min(2, mediaAspectRatio))
                        readonly property real stickerPreviewWidth: Math.max(
                          Style.space(72), Math.min(maximumMediaWidth,
                            Style.space(180) * stickerAspectRatio))

                        width: messageDelegate.mediaData
                          && messageDelegate.mediaData.kind === "video"
                          ? videoPreviewWidth + horizontalPadding
                          : messageDelegate.mediaData
                            && messageDelegate.mediaData.kind === "image"
                          ? imagePreviewWidth + horizontalPadding
                          : messageDelegate.isSticker
                          ? stickerPreviewWidth
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
                            ? borderTop + borderBottom
                            : (stickerOnlyMedia ? 0 : Style.space(16)))
                        y: dateDivider.height
                        x: modelData.from_me
                          ? messageDelegate.width - width - Style.space(18)
                          : (messageDelegate.showSenderAvatar
                            ? Style.space(56) : Style.space(18))
                        radius: borderOnlyMedia || stickerOnlyMedia
                          ? 0 : Style.cornerRadius + Style.space(6)
                        color: borderOnlyMedia || stickerOnlyMedia ? "transparent"
                          : (modelData.from_me
                            ? Style.selectedFillFor(root.foreground, root.accent)
                            : Style.normalFillFor(root.foreground, root.accent))
                        sourceBorderSpec: borderOnlyMedia
                          ? Border.flat(mediaBorderColor, 1)
                          : Border.none()

                        Column {
                          id: messageColumn
                          objectName: "messageColumn-"
                            + String(modelData.id || "")
                          anchors.left: parent.left
                          anchors.right: parent.right
                          anchors.leftMargin: bubble.borderOnlyMedia
                            ? bubble.borderLeft
                            : (bubble.stickerOnlyMedia ? 0 : Style.space(11))
                          anchors.rightMargin: bubble.borderOnlyMedia
                            ? bubble.borderRight
                            : (bubble.stickerOnlyMedia ? 0 : Style.space(11))
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
                            objectName: "messageText-" + String(modelData.id || "")
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
                              root.openMessageLink(link)
                            }

                            HoverHandler {
                              enabled: messageText.hoveredLink !== ""
                              cursorShape: Qt.PointingHandCursor
                            }
                          }
                          Text {
                            id: mediaCaptionText
                            objectName: "mediaCaptionText-"
                              + String(modelData.id || "")
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
                              root.openMessageLink(link)
                            }

                            HoverHandler {
                              enabled: mediaCaptionText.hoveredLink !== ""
                              cursorShape: Qt.PointingHandCursor
                            }
                          }
                          Item {
                            id: pollCard
                            objectName: "pollCard-" + String(modelData.id || "")
                            readonly property bool ended:
                              Number(messageDelegate.mediaData
                                ? messageDelegate.mediaData.end_timestamp || 0 : 0) > 0
                              && Number(messageDelegate.mediaData.end_timestamp)
                                <= root.currentTimestamp
                            readonly property int totalVoters:
                              Number(messageDelegate.mediaData
                                ? messageDelegate.mediaData.total_voters || 0 : 0)
                            visible: messageDelegate.isPoll
                            width: parent.width
                            height: visible ? pollContent.implicitHeight : 0

                            Column {
                              id: pollContent
                              width: parent.width
                              spacing: Style.space(7)

                              Text {
                                width: parent.width
                                text: messageDelegate.mediaData
                                  ? String(messageDelegate.mediaData.question || "Poll") : "Poll"
                                color: root.foreground
                                font.family: root.fontFamily
                                font.pixelSize: Style.font.body
                                font.bold: true
                                wrapMode: Text.WrapAtWordBoundaryOrAnywhere
                                textFormat: Text.PlainText
                              }

                              Text {
                                width: parent.width
                                text: Number(messageDelegate.mediaData
                                  ? messageDelegate.mediaData.selectable_count || 1 : 1) > 1
                                  ? "Select one or more" : "Select one"
                                color: root.sidebarSecondary
                                font.family: root.fontFamily
                                font.pixelSize: root.messageMetaFontSize
                              }

                              Repeater {
                                model: messageDelegate.mediaData
                                  && Array.isArray(messageDelegate.mediaData.options)
                                  ? messageDelegate.mediaData.options : []

                                delegate: Item {
                                  id: pollOption
                                  objectName: "pollOption-" + String(index)
                                  required property var modelData
                                  required property int index
                                  readonly property bool selected:
                                    modelData.selected_by_me === true
                                  readonly property int votes:
                                    Number(modelData.votes || 0)
                                  width: pollContent.width
                                  height: Math.max(Style.space(38),
                                    pollOptionLabel.implicitHeight + Style.space(16))

                                  CrispBorderSurface {
                                    anchors.fill: parent
                                    radius: Style.cornerRadius + Style.space(2)
                                    color: pollOption.selected
                                      ? Style.selectedFillFor(root.foreground, root.accent)
                                      : Style.normalFillFor(root.foreground, root.accent)
                                    sourceBorderSpec: Border.flat(
                                      pollOption.selected ? root.accent
                                        : Style.normalBorderFor(root.foreground, root.accent),
                                      Math.max(1, Style.normalBorderWidth))

                                    Rectangle {
                                      anchors.left: parent.left
                                      anchors.bottom: parent.bottom
                                      height: Math.max(1, Style.normalBorderWidth * 2)
                                      width: pollCard.totalVoters > 0
                                        ? parent.width * Math.min(1,
                                          pollOption.votes / pollCard.totalVoters) : 0
                                      color: root.accent
                                      opacity: 0.75
                                    }
                                  }

                                  Text {
                                    id: pollOptionLabel
                                    anchors.left: parent.left
                                    anchors.right: pollOptionCount.left
                                    anchors.leftMargin: Style.space(10)
                                    anchors.rightMargin: Style.space(8)
                                    anchors.verticalCenter: parent.verticalCenter
                                    text: (pollOption.selected ? "✓  " : "")
                                      + String(pollOption.modelData.name || "")
                                    color: root.foreground
                                    font.family: root.fontFamily
                                    font.pixelSize: Style.font.body
                                    wrapMode: Text.WrapAtWordBoundaryOrAnywhere
                                    textFormat: Text.PlainText
                                  }

                                  Text {
                                    id: pollOptionCount
                                    anchors.right: parent.right
                                    anchors.rightMargin: Style.space(10)
                                    anchors.verticalCenter: parent.verticalCenter
                                    text: String(pollOption.votes)
                                    color: root.sidebarSecondary
                                    font.family: root.fontFamily
                                    font.pixelSize: root.messageMetaFontSize
                                  }

                                  MouseArea {
                                    objectName: "pollOptionMouse-" + String(pollOption.index)
                                    anchors.fill: parent
                                    enabled: !pollCard.ended && root.service
                                      && !root.service.pollVotePending(messageDelegate.modelData)
                                    cursorShape: enabled
                                      ? Qt.PointingHandCursor : Qt.ArrowCursor
                                    onClicked: messageDelegate.togglePollOption(pollOption.index)
                                  }
                                }
                              }

                              Text {
                                width: parent.width
                                text: pollCard.ended ? "Poll ended"
                                  : (pollCard.totalVoters === 1 ? "1 vote"
                                    : pollCard.totalVoters + " votes")
                                color: pollCard.ended ? Color.urgent : root.sidebarSecondary
                                font.family: root.fontFamily
                                font.pixelSize: root.messageMetaFontSize
                              }
                            }
                          }
                          Item {
                            id: stickerCard
                            objectName: "stickerCard-" + String(modelData.id || "")
                            readonly property bool active: messageDelegate.isSticker
                            readonly property bool downloaded:
                              messageDelegate.mediaData
                              ? messageDelegate.mediaData.downloaded === true : false
                            readonly property bool lottie: messageDelegate.mediaData
                              ? messageDelegate.mediaData.lottie === true : false
                            readonly property string mediaPath:
                              messageDelegate.mediaData
                              ? String(messageDelegate.mediaData.path || "") : ""
                            readonly property string thumbnailPath:
                              messageDelegate.mediaData
                              ? String(messageDelegate.mediaData.thumbnail_path || "") : ""
                            readonly property string displayPath: downloaded
                              ? mediaPath : thumbnailPath
                            visible: active
                            width: parent.width
                            height: active ? width / bubble.stickerAspectRatio : 0

                            AnimatedImage {
                              id: stickerImage
                              objectName: "stickerImage-" + String(modelData.id || "")
                              anchors.fill: parent
                              source: stickerCard.active && root.service
                                ? root.service.fileUrl(stickerCard.displayPath,
                                  root.service.messageMediaRevision(modelData)) : ""
                              asynchronous: true
                              cache: false
                              fillMode: Image.PreserveAspectFit
                              onStatusChanged: {
                                if (status === AnimatedImage.Ready)
                                  root.scheduleMediaDownloadAnchorRestore(
                                    modelData.id)
                              }
                            }

                            Text {
                              anchors.centerIn: parent
                              width: parent.width
                              visible: String(stickerImage.source) === ""
                                || stickerImage.status === AnimatedImage.Error
                              text: messageDelegate.mediaData
                                && messageDelegate.mediaData.accessibility_label
                                ? String(messageDelegate.mediaData.accessibility_label)
                                : (stickerCard.lottie
                                  ? "Lottie sticker unavailable" : "Sticker unavailable")
                              color: root.muted
                              font.family: root.fontFamily
                              font.pixelSize: Style.font.caption
                              wrapMode: Text.WrapAtWordBoundaryOrAnywhere
                              horizontalAlignment: Text.AlignHCenter
                            }

                            Text {
                              anchors.right: parent.right
                              anchors.bottom: parent.bottom
                              anchors.margins: Style.space(4)
                              visible: stickerCard.lottie
                                && stickerImage.status === AnimatedImage.Ready
                              text: "Animated sticker"
                              color: root.foreground
                              font.family: root.fontFamily
                              font.pixelSize: root.messageMetaFontSize
                            }

                            Text {
                              objectName: "stickerDownloadStatus-"
                                + String(modelData.id || "")
                              readonly property bool active: stickerCard.active
                                && !stickerCard.lottie && !stickerCard.downloaded
                                && root.service
                                && root.service.mediaDownloading(modelData)
                              anchors.centerIn: parent
                              visible: active
                              text: "󰔟"
                              color: root.foreground
                              font.family: root.fontFamily
                              font.pixelSize: Style.font.icon * 1.5
                            }
                          }
                          Item {
                            id: mediaPreviewCard
                            objectName: "mediaPreviewCard-"
                              + String(modelData.id || "")
                            readonly property bool isImage:
                              messageDelegate.mediaData
                              && messageDelegate.mediaData.kind === "image"
                            property alias videoSurface: inlineVideoOutput
                            readonly property bool isVideo: messageDelegate.mediaData
                              && messageDelegate.mediaData.kind === "video"
                            readonly property bool isGif: isVideo
                              && messageDelegate.mediaData.gif_playback === true
                            readonly property real topMargin: Style.space(8)
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
                              ? topMargin + width / (isVideo
                                  ? bubble.mediaAspectRatio
                                  : bubble.imageAspectRatio)
                              : 0

                            Rectangle {
                              id: mediaPreviewMask
                              objectName: "mediaPreviewMask-"
                                + String(modelData.id || "")
                              anchors.fill: parent
                              anchors.topMargin: mediaPreviewCard.topMargin
                              radius: bubble.radius
                              visible: false
                              layer.enabled: true
                            }

                            Image {
                              id: mediaPreviewImage
                              objectName: "mediaPreviewImage-"
                                + String(modelData.id || "")
                              anchors.fill: parent
                              anchors.topMargin: mediaPreviewCard.topMargin
                              visible: !mediaPreviewCard.inlineActive
                              source: mediaPreviewCard.visible && root.service
                                ? root.service.fileUrl(
                                  mediaPreviewCard.displayPath,
                                  root.service.messageMediaRevision(modelData)) : ""
                              asynchronous: true
                              cache: false
                              fillMode: Image.PreserveAspectFit
                              layer.enabled: mediaPreviewCard.isImage
                              layer.smooth: true
                              layer.effect: MultiEffect {
                                maskEnabled: true
                                maskSource: mediaPreviewMask
                                maskThresholdMin: 0.5
                                maskSpreadAtMin: 1.0
                              }
                              onStatusChanged: {
                                if (status === Image.Ready)
                                  root.scheduleMediaDownloadAnchorRestore(
                                    modelData.id)
                              }
                            }

                            VideoOutput {
                              id: inlineVideoOutput
                              anchors.fill: parent
                              anchors.topMargin: mediaPreviewCard.topMargin
                              visible: mediaPreviewCard.inlineActive
                              fillMode: VideoOutput.PreserveAspectFit
                              endOfStreamPolicy: VideoOutput.KeepLastFrame
                            }

                            HoverHandler {
                              id: mediaPreviewHover
                            }

                            MouseArea {
                              anchors.fill: parent
                              anchors.topMargin: mediaPreviewCard.topMargin
                              enabled: mediaPreviewCard.downloaded
                              cursorShape: enabled
                                ? Qt.PointingHandCursor : Qt.ArrowCursor
                              onClicked: {
                                if (mediaPreviewCard.isVideo)
                                  root.toggleInlineVideo(mediaPreviewCard)
                                else if (root.service)
                                  root.openImagePreview(
                                    mediaPreviewCard.mediaPath,
                                    root.service.messageMediaRevision(modelData))
                              }
                            }

                            Text {
                              anchors.centerIn: parent
                              anchors.verticalCenterOffset:
                                mediaPreviewCard.topMargin / 2
                              visible: mediaPreviewCard.isVideo
                                && !mediaPreviewCard.inlineActive
                                && mediaPreviewImage.status !== Image.Ready
                              text: "󰕧"
                              color: root.muted
                              font.family: root.fontFamily
                              font.pixelSize: Style.font.displayLarge
                            }

                            CrispButton {
                              objectName: "mediaDownloadButton-"
                                + String(modelData.id || "")
                              readonly property bool downloading: visible
                                && root.service
                                && root.service.mediaDownloading(modelData)

                              anchors.centerIn: parent
                              anchors.verticalCenterOffset:
                                mediaPreviewCard.topMargin / 2
                              visible: messageDelegate.mediaData
                                && mediaPreviewCard.visible
                                && (mediaPreviewCard.isVideo
                                  || !mediaPreviewCard.downloaded)
                              opacity: mediaPreviewCard.isVideo
                                && mediaPreviewCard.downloaded
                                ? (mediaPreviewHover.hovered ? 1 : 0) : 1
                              width: Style.space(40)
                              height: Style.space(40)
                              iconSize: Style.font.icon * 1.5
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
                                  root.downloadMedia(modelData, messageDelegate)
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
                                  root.downloadMedia(modelData, messageDelegate)
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

                      Row {
                        id: messageFooter
                        visible: messageDelegate.showMessageTime
                        anchors.top: reactionsBar.visible
                          ? reactionsBar.bottom : bubble.bottom
                        anchors.topMargin: Style.space(3)
                        x: modelData.from_me
                          ? bubble.x + bubble.width - width - Style.space(4)
                          : bubble.x + Style.space(4)
                        spacing: 8

                        Item {
                          id: messageReceiptHoverTarget
                          objectName: "messageReceiptHoverTarget-"
                            + String(modelData.id || "")
                          visible: modelData.from_me === true
                          anchors.verticalCenter: parent.verticalCenter
                          width: messageReceiptStatus.implicitWidth
                          height: messageReceiptStatus.implicitHeight

                          Text {
                            id: messageReceiptStatus
                            objectName: "messageReceiptStatus-"
                              + String(modelData.id || "")
                            readonly property string receiptTooltipText:
                              root.messageReceiptTooltip(modelData)
                            readonly property var receiptTooltipGroups:
                              root.messageReceiptGroups(modelData)
                            readonly property bool receiptTooltipVisible:
                              messageReceiptTooltipPopup.visible
                            readonly property var receiptTooltipControl:
                              messageReceiptTooltipPopup
                            readonly property bool receiptHovered:
                              messageReceiptHoverArea.containsMouse
                            anchors.centerIn: parent
                            text: root.messageReceiptIcon(modelData.receipt)
                            color: Number(modelData.receipt || 0) >= 3
                              ? root.accent : root.timestamp
                            font.family: root.fontFamily
                            font.pixelSize: 14
                            font.letterSpacing:
                              Number(modelData.receipt || 0) >= 2
                                && Number(modelData.receipt || 0) < 4
                              ? -3 : 0
                            font.bold: Number(modelData.receipt || 0) > 0
                            Accessible.name:
                              root.messageReceiptLabel(modelData.receipt)
                          }

                          MouseArea {
                            id: messageReceiptHoverArea
                            objectName: "messageReceiptHoverArea-"
                              + String(modelData.id || "")
                            anchors.fill: parent
                            anchors.margins: -3
                            acceptedButtons: Qt.NoButton
                            hoverEnabled: true
                          }

                          QQC.ToolTip {
                            id: messageReceiptTooltipPopup
                            objectName: "messageReceiptTooltip-"
                              + String(modelData.id || "")
                            readonly property color tooltipBackground:
                              Color.tooltip.background
                            readonly property color tooltipForeground:
                              Color.tooltip.text
                            readonly property color tooltipBorder:
                              Color.tooltip.border
                            readonly property var tooltipBorderSpec:
                              Border.localOrSurfaceSpec("tooltip", "border",
                                tooltipBorder, Color.tooltip.border,
                                Math.max(1, Style.normalBorderWidth))
                            visible: messageReceiptHoverArea.containsMouse
                            text: messageReceiptStatus.receiptTooltipText
                            delay: 400
                            timeout: -1
                            padding: 0

                            background: BorderSurface {
                              color: messageReceiptTooltipPopup.tooltipBackground
                              borderSpec:
                                messageReceiptTooltipPopup.tooltipBorderSpec
                              radius: 0
                            }

                            contentItem: Item {
                              id: messageReceiptTooltipBody
                              readonly property var groups:
                                messageReceiptStatus.receiptTooltipGroups
                              readonly property color headerColor: Qt.rgba(
                                messageReceiptTooltipPopup.tooltipForeground.r,
                                messageReceiptTooltipPopup.tooltipForeground.g,
                                messageReceiptTooltipPopup.tooltipForeground.b,
                                messageReceiptTooltipPopup.tooltipForeground.a
                                  * 0.72)
                              readonly property color detailColor:
                                messageReceiptTooltipPopup.tooltipForeground
                              readonly property real groupSpacing: Style.space(6)
                              readonly property string contentFontFamily:
                                root.fontFamily
                              readonly property real contentFontSize:
                                Style.font.bodySmall
                              readonly property real leftInset: Border.left(
                                messageReceiptTooltipPopup.tooltipBorderSpec)
                                + Style.spacing.controlPaddingX
                              readonly property real rightInset: Border.right(
                                messageReceiptTooltipPopup.tooltipBorderSpec)
                                + Style.spacing.controlPaddingX
                              readonly property real topInset: Border.top(
                                messageReceiptTooltipPopup.tooltipBorderSpec)
                                + Style.spacing.controlPaddingY
                              readonly property real bottomInset: Border.bottom(
                                messageReceiptTooltipPopup.tooltipBorderSpec)
                                + Style.spacing.controlPaddingY

                              implicitWidth:
                                messageReceiptTooltipContent.implicitWidth
                                + leftInset + rightInset
                              implicitHeight:
                                messageReceiptTooltipContent.implicitHeight
                                  + topInset + bottomInset

                              Column {
                                id: messageReceiptTooltipContent
                                x: parent.leftInset
                                y: parent.topInset
                                spacing: messageReceiptTooltipBody.groupSpacing

                                Repeater {
                                  model: messageReceiptTooltipBody.groups

                                  delegate: Column {
                                    required property var modelData
                                    spacing: 0

                                    Text {
                                      text: parent.modelData.label
                                      textFormat: Text.PlainText
                                      color:
                                        messageReceiptTooltipBody.headerColor
                                      font.family: messageReceiptTooltipBody
                                        .contentFontFamily
                                      font.pixelSize: messageReceiptTooltipBody
                                        .contentFontSize
                                    }

                                    Repeater {
                                      model: parent.modelData.entries

                                      delegate: Text {
                                        required property var modelData
                                        text: String(modelData || "")
                                        textFormat: Text.PlainText
                                        color:
                                          messageReceiptTooltipBody.detailColor
                                        font.family: messageReceiptTooltipBody
                                          .contentFontFamily
                                        font.pixelSize: messageReceiptTooltipBody
                                          .contentFontSize
                                      }
                                    }
                                  }
                                }
                              }
                            }
                          }
                        }

                        Text {
                          id: messageFooterTime
                          objectName: "messageTimestamp-"
                            + String(modelData.id || "")
                          anchors.verticalCenter: parent.verticalCenter
                          text: Model.messageTime(modelData.timestamp,
                            root.messageTimeFormat)
                          color: root.timestamp
                          font.family: root.fontFamily
                          font.pixelSize: 10
                        }
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
                      id: messageComposerControls

                      readonly property real controlHeight:
                        root.snapToDevicePixel(composer.implicitHeight)

                      anchors.left: parent.left
                      anchors.right: parent.right
                      anchors.leftMargin: Style.space(14)
                      anchors.rightMargin: Style.space(14)
                      anchors.verticalCenter: parent.verticalCenter
                      spacing: Style.space(8)
                      visible: !root.voiceRecordingActive && !root.voiceOutboxEntry
                        && !root.textOutboxEntry
                      CrispTextField {
                        id: composer
                        objectName: "composer"
                        height: messageComposerControls.controlHeight
                        width: parent.width - pollButton.width
                          - voiceRecordButton.width - sendButton.width
                          - parent.spacing * 3
                        enabled: root.service && root.service.selectedChatJid !== ""
                        placeholderText: enabled ? "Message" : "Select a conversation"
                        onTextChanged: if (root.service
                            && typeof root.service.noteComposerActivity === "function")
                          root.service.noteComposerActivity(text)
                        onAccepted: root.submitMessage()
                      }
                      SquareControlButton {
                        id: pollButton
                        objectName: "pollButton"
                        controlHeight: messageComposerControls.controlHeight
                        centeredIconText: "󰘻"
                        bordered: true
                        enabled: composer.enabled
                        foreground: root.foreground
                        tooltipText: "Create poll"
                        onClicked: root.openPollCreator()
                      }
                      SquareControlButton {
                        id: voiceRecordButton
                        objectName: "voiceRecordButton"
                        controlHeight: messageComposerControls.controlHeight
                        centeredIconText: "󰍬"
                        bordered: true
                        enabled: composer.enabled && root.service
                          && !root.voiceOutboxEntry
                          && Number(root.service.voiceMessageRequestId || 0) === 0
                        foreground: root.foreground
                        tooltipText: "Record voice message"
                        onClicked: root.startVoiceRecording()
                      }
                      SquareControlButton {
                        id: sendButton
                        objectName: "sendButton"
                        controlHeight: messageComposerControls.controlHeight
                        centeredIconText: "󰒊"
                        bordered: true
                        enabled: composer.enabled
                        foreground: root.foreground
                        tooltipText: "Send message"
                        onClicked: root.submitMessage()
                      }
                    }
                    Row {
                      id: textOutboxControls
                      objectName: "textOutboxControls"
                      anchors.left: parent.left
                      anchors.right: parent.right
                      anchors.leftMargin: Style.space(14)
                      anchors.rightMargin: Style.space(14)
                      anchors.verticalCenter: parent.verticalCenter
                      spacing: Style.space(8)
                      visible: !root.voiceRecordingActive && !root.voiceOutboxEntry
                        && !!root.textOutboxEntry

                      Text {
                        id: textOutboxStatus
                        objectName: "textOutboxStatus"
                        anchors.verticalCenter: parent.verticalCenter
                        text: {
                          if (!root.textOutboxEntry) return ""
                          var preview = String(root.textOutboxEntry.text || "")
                          if (preview.length > 52) preview = preview.substring(0, 51) + "…"
                          return root.textOutboxEntry.status === "failed"
                            ? "Message failed  " + preview : "Sending message…  " + preview
                        }
                        elide: Text.ElideRight
                        color: root.textOutboxEntry
                          && root.textOutboxEntry.status === "failed"
                          ? Color.urgent : root.foreground
                        font.family: root.fontFamily
                        font.pixelSize: Style.font.body
                        width: Math.max(0, parent.width - textRetryButton.width
                          - textOutboxDiscardButton.width - parent.spacing * 2)
                      }
                      CrispButton {
                        id: textOutboxDiscardButton
                        objectName: "textOutboxDiscardButton"
                        iconText: "󰅖"
                        bordered: true
                        visible: !!root.textOutboxEntry
                          && root.textOutboxEntry.status !== "sending"
                        enabled: visible && root.service !== null
                        foreground: root.foreground
                        tooltipText: "Discard pending message"
                        onClicked: root.service.discardTextMessage(root.textOutboxEntry)
                      }
                      CrispButton {
                        id: textRetryButton
                        objectName: "textRetryButton"
                        iconText: "󰑐"
                        bordered: true
                        visible: !!root.textOutboxEntry
                          && root.textOutboxEntry.status === "failed"
                        enabled: visible && root.service !== null
                          && root.service.connectionState === "connected"
                        foreground: root.foreground
                        tooltipText: root.textOutboxEntry
                          ? String(root.textOutboxEntry.error || "Retry message") : ""
                        onClicked: root.service.retryTextMessage(root.textOutboxEntry)
                      }
                    }
                    Row {
                      id: voiceOutboxControls
                      objectName: "voiceOutboxControls"
                      anchors.left: parent.left
                      anchors.right: parent.right
                      anchors.leftMargin: Style.space(14)
                      anchors.rightMargin: Style.space(14)
                      anchors.verticalCenter: parent.verticalCenter
                      spacing: Style.space(8)
                      visible: !root.voiceRecordingActive
                        && !!root.voiceOutboxEntry

                      Text {
                        id: voiceOutboxStatus
                        objectName: "voiceOutboxStatus"
                        anchors.verticalCenter: parent.verticalCenter
                        text: {
                          if (!root.voiceOutboxEntry) return ""
                          var duration = Model.mediaDuration(Math.floor(
                            Number(root.voiceOutboxEntry.duration_ms || 0) / 1000))
                          return root.voiceOutboxEntry.status === "sending"
                            ? "Sending voice message…  " + duration
                            : "Voice message failed  " + duration
                        }
                        color: root.voiceOutboxEntry
                          && root.voiceOutboxEntry.status === "failed"
                          ? Color.urgent : root.foreground
                        font.family: root.fontFamily
                        font.pixelSize: Style.font.body
                      }
                      Item {
                        width: Math.max(0, parent.width - voiceOutboxStatus.width
                          - voiceRetryButton.width - voiceOutboxDiscardButton.width
                          - parent.spacing * 3)
                        height: 1
                      }
                      CrispButton {
                        id: voiceOutboxDiscardButton
                        objectName: "voiceOutboxDiscardButton"
                        iconText: "󰅖"
                        bordered: true
                        visible: !!root.voiceOutboxEntry
                          && root.voiceOutboxEntry.status === "failed"
                        enabled: !!root.voiceOutboxEntry
                          && root.voiceOutboxEntry.status === "failed"
                          && root.service !== null
                        foreground: root.foreground
                        tooltipText: "Discard failed voice message"
                        onClicked: root.service.discardVoiceRecording(
                          root.voiceOutboxEntry.recording_id)
                      }
                      CrispButton {
                        id: voiceRetryButton
                        objectName: "voiceRetryButton"
                        iconText: "󰑐"
                        bordered: true
                        visible: !!root.voiceOutboxEntry
                          && root.voiceOutboxEntry.status === "failed"
                        enabled: !!root.voiceOutboxEntry
                          && root.voiceOutboxEntry.status === "failed"
                          && root.service !== null
                          && root.service.connectionState === "connected"
                          && Number(root.service.voiceMessageRequestId || 0) === 0
                        foreground: root.foreground
                        tooltipText: root.voiceOutboxEntry
                          ? String(root.voiceOutboxEntry.error || "Retry voice message") : ""
                        onClicked: root.service.retryVoiceMessage(root.voiceOutboxEntry)
                      }
                    }
                    Row {
                      id: voiceRecordingControls
                      objectName: "voiceRecordingControls"
                      anchors.left: parent.left
                      anchors.right: parent.right
                      anchors.leftMargin: Style.space(14)
                      anchors.rightMargin: Style.space(14)
                      anchors.verticalCenter: parent.verticalCenter
                      spacing: Style.space(8)
                      visible: root.voiceRecordingActive

                      Item {
                        width: Math.max(0, parent.width
                          - voiceRecordingStatus.width - voiceCancelButton.width
                          - voiceSendButton.width - parent.spacing * 3)
                        height: 1
                      }
                      Text {
                        id: voiceRecordingStatus
                        objectName: "voiceRecordingStatus"
                        anchors.verticalCenter: parent.verticalCenter
                        text: "●  " + Model.mediaDuration(
                          Math.floor(root.voiceRecordingDurationMs / 1000))
                        color: Color.urgent
                        font.family: root.fontFamily
                        font.pixelSize: Style.font.body
                      }
                      SquareControlButton {
                        id: voiceCancelButton
                        objectName: "voiceCancelButton"
                        controlHeight: messageComposerControls.controlHeight
                        centeredIconText: "󰅖"
                        bordered: true
                        foreground: root.foreground
                        tooltipText: "Discard voice message"
                        onClicked: root.stopVoiceRecording(false)
                      }
                      SquareControlButton {
                        id: voiceSendButton
                        objectName: "voiceSendButton"
                        controlHeight: messageComposerControls.controlHeight
                        centeredIconText: "󰒊"
                        bordered: true
                        enabled: root.voiceRecordingDurationMs >= 250
                        foreground: root.foreground
                        tooltipText: "Send voice message"
                        onClicked: root.stopVoiceRecording(true)
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
                objectName: "daemonSetupTitle"
                anchors.horizontalCenter: parent.horizontalCenter
                text: root.daemonSetupRequired
                  ? "Set up WhatsApp"
                  : (root.pairing ? "Link WhatsApp" : "Connecting to WhatsApp")
                color: root.foreground
                font.family: root.fontFamily
                font.pixelSize: Style.font.displayLarge
                font.bold: true
              }
              Text {
                anchors.horizontalCenter: parent.horizontalCenter
                width: parent.width
                horizontalAlignment: Text.AlignHCenter
                text: root.daemonSetupRequired
                  ? (root.daemonSetupBusy
                    ? String(root.service.daemonSetupDetail
                      || "Building the background service…")
                    : "Build and install the background service on this computer.")
                  : (root.pairing
                    ? "On your phone, open WhatsApp → Linked devices → Link a device"
                    : (root.service && root.service.connectionDetail
                      ? root.service.connectionDetail : "The background service is starting…"))
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
              Text {
                visible: root.daemonSetupRequired
                anchors.horizontalCenter: parent.horizontalCenter
                width: parent.width
                horizontalAlignment: Text.AlignHCenter
                text: "The first local build can take several minutes and requires mise or Rust."
                color: root.sidebarSecondary
                font.family: root.fontFamily
                font.pixelSize: Style.font.caption
                wrapMode: Text.Wrap
              }
              CrispButton {
                objectName: "daemonSetupButton"
                visible: root.daemonSetupRequired
                anchors.horizontalCenter: parent.horizontalCenter
                text: root.daemonSetupBusy
                  ? "Setting up daemon…" : "Build and start daemon"
                iconText: root.daemonSetupBusy ? "󰔟" : "󰒓"
                bordered: true
                active: !root.daemonSetupBusy
                enabled: !root.daemonSetupBusy
                foreground: root.foreground
                onClicked: if (root.service) root.service.setupDaemonRuntime()
              }
              CrispButton {
                objectName: "daemonRetryButton"
                visible: !root.pairing && !root.daemonSetupRequired
                anchors.horizontalCenter: parent.horizontalCenter
                text: "Try again"
                iconText: "󰑐"
                bordered: true
                foreground: root.foreground
                onClicked: if (root.service) root.service.retryDaemon()
              }
            }
          }

          CrispBorderSurface {
            visible: root.visibleError !== ""
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
              text: root.visibleError
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
