import QtQuick
import "../../../quickshell/Model.js" as Model

QtObject {
  id: root

  property string connectionState: "connected"
  property string connectionDetail: ""
  property string qrImageUrl: ""
  property int unreadTotal: 0
  property string lastError: ""
  property string lastErrorRequestId: ""
  property string chatStateResyncStatus: "idle"
  property string chatStateResyncMessage: ""
  readonly property bool chatStateResyncBusy:
    chatStateResyncStatus === "requested" || chatStateResyncStatus === "syncing"
  property bool daemonSetupRequired: false
  property bool daemonSetupBusy: false
  property string daemonSetupDetail: ""
  property string daemonSetupError: ""
  property bool unreadOnly: false
  property var chats: []
  property var messages: []
  property string messagesChatJid: ""
  property string messagesFirstUnreadId: ""
  property bool messagesResponseHasFollowup: false
  property int messagesResponseSerial: 0
  property int messagesNavigationSerial: 0
  property int messageSentSerial: 0
  property int incomingMessageSerial: 0
  property int pollCreateRequestId: 0
  property int voiceMessageRequestId: 0
  property bool voiceRecordingTestMode: true
  property int voiceRecordingTestDurationMs: 2400
  property int voiceRecordingSerial: 0
  property var voiceOutboxEntries: []
  property var textOutboxEntries: []
  property var stagedImage: null
  property bool pasteImageEmpty: false
  property var sentImages: []
  property var pollVotes: []
  property var createdPolls: []
  property bool pollVotePendingValue: false
  property string selectedChatJid: ""
  property var groupParticipants: []
  property string groupParticipantsChatJid: ""
  property string groupParticipantsError: ""
  property var mediaDownloadRequests: ({})
  property var avatarUrls: ({})
  property var calls: []
  property var sentMessages: []
  property var sentMentionMessages: []
  property var replyTarget: null
  property var sentReplies: []
  onSelectedChatJidChanged: replyTarget = null
  property var sentVoiceMessages: []
  property var discardedVoiceRecordings: []
  property var pinnedChats: []
  property var downloadedMessages: []
  property var chatStateUpdates: []
  property string chatStateLabelResult: ""
  property var chatStateLabels: ({})
  property string presenceLabelResult: ""
  readonly property var selectedChat: {
    for (var i = 0; i < chats.length; i++)
      if (String(chats[i].jid || "") === selectedChatJid) return chats[i]
    return null
  }

  signal messagesWillChange(bool preservePosition)
  signal textMessageAccepted(string deliveryId, string chatJid, string text)
  signal clipboardTextPasteRequested()

  function record(name, value) {
    calls = calls.concat([{ name: name, value: value }])
  }

  function refreshMetadata() { record("refreshMetadata", null) }

  function setupDaemonRuntime() {
    if (daemonSetupBusy) return false
    daemonSetupBusy = true
    record("setupDaemonRuntime", null)
    return true
  }

  function retryDaemon() { record("retryDaemon", null) }

  function ensureDirectChat(jid, name) {
    var value = String(jid || "")
    if (!value) return false
    for (var i = 0; i < chats.length; i++)
      if (String((chats[i] || {}).jid || "") === value) return true
    chats = [{
      jid: value,
      name: String(name || ""),
      phone_number: value.split("@")[0],
      last_message: "",
      last_sender_name: "",
      last_timestamp: 0,
      unread: 0,
      pinned: false,
      muted: false,
      is_group: false
    }].concat(chats)
    return true
  }

  function receiveChats(value) {
    var incoming = Array.isArray(value) ? value.slice() : []
    var selected = selectedChat
    if (selected && selected.jid) {
      var found = false
      for (var i = 0; i < incoming.length; i++)
        if (String((incoming[i] || {}).jid || "") === String(selected.jid)) found = true
      if (!found) incoming = [selected].concat(incoming)
    }
    chats = incoming
  }

  function requestChatStateResync() {
    if (connectionState !== "connected" || chatStateResyncBusy) return false
    chatStateResyncStatus = "requested"
    chatStateResyncMessage = "Chat-state resync requested"
    record("requestChatStateResync", null)
    return true
  }

  function selectChat(jid) {
    var value = String(jid || "")
    var changed = value !== selectedChatJid
    selectedChatJid = value
    if (changed) {
      messagesChatJid = ""
      messages = []
      messagesNavigationSerial++
    }
    record("selectChat", selectedChatJid)
  }

  function setPanelState(visible, focused) {
    record("setPanelState", { visible: visible, focused: focused })
  }

  function noteComposerActivity(text) {
    chatStateUpdates = chatStateUpdates.concat([String(text || "") ? "typing" : "paused"])
    return String(text || "") !== ""
  }

  function newVoiceRecording() {
    voiceRecordingSerial++
    var recordingId = "42-" + voiceRecordingSerial
    return {
      recording_id: recordingId,
      chat_jid: selectedChatJid,
      path: "/synthetic/outbox/voice-" + recordingId + ".ogg"
    }
  }

  function beginVoiceRecording() {
    chatStateUpdates = chatStateUpdates.concat(["recording"])
    return selectedChatJid !== "" && voiceMessageRequestId === 0
  }

  function finishVoiceRecording() {
    chatStateUpdates = chatStateUpdates.concat(["paused"])
    return true
  }

  function sendVoiceMessage(recordingId, chatJid, durationMs) {
    if (!chatJid || !recordingId || Number(durationMs || 0) < 250)
      return false
    sentVoiceMessages = sentVoiceMessages.concat([{
      chat_jid: String(chatJid),
      recording_id: String(recordingId),
      duration_ms: Number(durationMs)
    }])
    messageSentSerial++
    return true
  }

  function retryVoiceMessage(entry) {
    if (!sendVoiceMessage(entry.recording_id, entry.chat_jid, entry.duration_ms))
      return false
    voiceOutboxEntries = voiceOutboxEntries.filter(function(value) {
      return String(value.recording_id || "") !== String(entry.recording_id || "")
    })
    return true
  }

  function discardVoiceRecording(recordingId) {
    discardedVoiceRecordings = discardedVoiceRecordings.concat([
      String(recordingId || "")
    ])
    voiceOutboxEntries = voiceOutboxEntries.filter(function(entry) {
      return String(entry.recording_id || "") !== String(recordingId || "")
    })
    return true
  }

  function textOutboxForChat(chatJid) {
    for (var i = textOutboxEntries.length - 1; i >= 0; i--)
      if (String(textOutboxEntries[i].chat_jid || "") === String(chatJid || ""))
        if (String(textOutboxEntries[i].status || "") === "failed")
          return textOutboxEntries[i]
    return null
  }

  function retryTextMessage(entry) {
    if (!entry || entry.status !== "failed") return false
    textOutboxEntries = textOutboxEntries.filter(function(value) {
      return value.delivery_id !== entry.delivery_id
    })
    return true
  }

  function discardTextMessage(entry) {
    if (!entry || entry.status === "sending") return false
    textOutboxEntries = textOutboxEntries.filter(function(value) {
      return value.delivery_id !== entry.delivery_id
    })
    return true
  }

  function chatStateLabel(jid) {
    return String(chatStateLabels[String(jid || "")] || chatStateLabelResult)
  }
  function presenceLabel() { return presenceLabelResult }

  function setUnreadOnly(value) { unreadOnly = value === true }

  function beginReply(message) {
    if (!message || !message.id || !message.sender_jid || message.chat_jid !== selectedChatJid) return false
    replyTarget = { message_id: message.id, sender_jid: message.sender_jid,
      sender_name: message.from_me ? "You" : message.sender_name, text: message.text }
    return true
  }

  function sendMessage(text, selections) {
    var body = String(text || "")
    if (!selectedChatJid || !body.trim()) return false
    sentMessages = sentMessages.concat([body])
    sentMentionMessages = sentMentionMessages.concat([Model.composeMentions(body, selections)])
    sentReplies = sentReplies.concat([replyTarget])
    replyTarget = null
    messageSentSerial++
    textMessageAccepted(
      "fixture-" + String(messageSentSerial), selectedChatJid, body)
    return true
  }

  function stagedFixturePath() {
    return decodeURIComponent(
      String(Qt.resolvedUrl("pixel.svg")).substring("file://".length))
  }

  function pasteImage() {
    if (!selectedChatJid) return false
    record("pasteImage", null)
    if (pasteImageEmpty) {
      clipboardTextPasteRequested()
      return true
    }
    stagedImage = {
      path: stagedFixturePath(),
      width: 800,
      height: 600,
      mime_type: "image/png"
    }
    return true
  }

  function clearStagedImage() {
    stagedImage = null
  }

  function sendImageMessage(text) {
    var body = String(text || "")
    if (!selectedChatJid || !stagedImage || !stagedImage.path) return false
    sentReplies = sentReplies.concat([replyTarget])
    replyTarget = null
    messageSentSerial++
    var deliveryId = "fixture-image-" + String(messageSentSerial)
    sentImages = sentImages.concat([{
      chat_jid: selectedChatJid,
      path: String(stagedImage.path),
      text: body,
      delivery_id: deliveryId
    }])
    stagedImage = null
    textMessageAccepted(deliveryId, selectedChatJid, body)
    return true
  }

  function setChatPinned(jid, pinned) {
    var value = String(jid || "")
    if (!value) return false
    var updated = chats.slice()
    for (var i = 0; i < updated.length; i++) {
      if (String(updated[i].jid || "") !== value) continue
      var replacement = Object.assign({}, updated[i])
      replacement.pinned = pinned === true
      updated[i] = replacement
      chats = updated
      pinnedChats = pinnedChats.concat([{
        jid: value,
        pinned: pinned === true
      }])
      return true
    }
    return false
  }

  function createPoll(question, options, multipleAnswers) {
    createdPolls = createdPolls.concat([{
      question: String(question || ""),
      options: options.slice(),
      multipleAnswers: multipleAnswers === true
    }])
    return true
  }

  function pollVotePending() { return pollVotePendingValue }

  function votePoll(message, selectedOptions) {
    pollVotes = pollVotes.concat([{
      message: message,
      selectedOptions: selectedOptions.slice()
    }])
    return true
  }

  function loadMessages(items, firstUnreadId) {
    messagesWillChange(messagesChatJid === selectedChatJid
      && messages.length > 0)
    messages = items.slice()
    messagesChatJid = selectedChatJid
    messagesFirstUnreadId = String(firstUnreadId || "")
    messagesResponseHasFollowup = false
    messagesResponseSerial++
    for (var i = 0; i < messages.length; i++) {
      var media = messages[i] ? messages[i].media || null : null
      if (media && media.kind === "sticker" && media.downloaded !== true
          && media.lottie !== true) downloadMedia(messages[i])
    }
  }

  function replaceMessages(items, preservePosition) {
    messagesWillChange(preservePosition === true)
    messages = items.slice()
  }

  function downloadMedia(message) {
    downloadedMessages = downloadedMessages.concat([message])
    return true
  }

  function messageMedia(message) { return message ? message.media || null : null }
  function messageMediaRevision() { return "0-0" }
  function mediaDownloading(message) {
    var id = String(message ? message.id || "" : "")
    for (var i = 0; i < downloadedMessages.length; i++)
      if (String((downloadedMessages[i] || {}).id || "") === id) return true
    return false
  }
  function fileUrl(path, revision) {
    return path ? "file://" + String(path) + "?v=" + String(revision || 0) : ""
  }
  function avatarUrl(jid) { return String(avatarUrls[String(jid || "")] || "") }
  function requestAvatar(jid) { record("requestAvatar", String(jid || "")) }
  function refreshSelectedGroupParticipants() { return false }
  function reactToMessage() { return true }
  function openMap(latitude, longitude) {
    record("openMap", { latitude: latitude, longitude: longitude })
  }
  function openFile(path) { record("openFile", path) }
  function saveFile(path) { record("saveFile", path) }
  function unlinkDevice() { return connectionState === "connected" }
}
