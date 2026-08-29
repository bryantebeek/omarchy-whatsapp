import QtQuick

QtObject {
  id: root

  property string connectionState: "connected"
  property string connectionDetail: ""
  property string qrImageUrl: ""
  property int unreadTotal: 0
  property string lastError: ""
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
  property var pollVotes: []
  property var createdPolls: []
  property string selectedChatJid: ""
  property var groupParticipants: []
  property string groupParticipantsChatJid: ""
  property string groupParticipantsError: ""
  property var mediaDownloadRequests: ({})
  property var calls: []
  property var sentMessages: []
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

  function record(name, value) {
    calls = calls.concat([{ name: name, value: value }])
  }

  function refreshMetadata() { record("refreshMetadata", null) }

  function selectChat(jid) {
    selectedChatJid = String(jid || "")
    messagesChatJid = ""
    messages = []
    messagesNavigationSerial++
    record("selectChat", selectedChatJid)
  }

  function setPanelState(visible, focused) {
    record("setPanelState", { visible: visible, focused: focused })
  }

  function noteComposerActivity(text) {
    chatStateUpdates = chatStateUpdates.concat([String(text || "") ? "typing" : "paused"])
    return String(text || "") !== ""
  }

  function chatStateLabel(jid) {
    return String(chatStateLabels[String(jid || "")] || chatStateLabelResult)
  }
  function presenceLabel() { return presenceLabelResult }

  function setUnreadOnly(value) { unreadOnly = value === true }

  function sendMessage(text) {
    var body = String(text || "")
    if (!selectedChatJid || !body.trim()) return false
    sentMessages = sentMessages.concat([body])
    messageSentSerial++
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

  function pollVotePending() { return false }

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
  function avatarUrl() { return "" }
  function requestAvatar() {}
  function refreshSelectedGroupParticipants() { return false }
  function reactToMessage() { return true }
  function openMap() {}
  function openFile(path) { record("openFile", path) }
  function saveFile(path) { record("saveFile", path) }
  function unlinkDevice() { return connectionState === "connected" }
}
