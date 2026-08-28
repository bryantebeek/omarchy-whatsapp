import QtQuick
import Quickshell
import Quickshell.Io
import "Model.js" as Model

Item {
  id: root

  visible: false
  width: 0
  height: 0

  property var shell: null
  property var manifest: null

  readonly property string pluginId: manifest && manifest.id
    ? String(manifest.id) : "io.github.bryantebeek.whatsapp"
  readonly property string pluginDir: manifest && manifest.__sourceDir
    ? String(manifest.__sourceDir) : ""
  readonly property string daemonSetupScript: pluginDir
    ? pluginDir + "/scripts/setup-daemon.sh" : ""
  readonly property string socketPath: {
    var runtime = Quickshell.env("XDG_RUNTIME_DIR")
    return String(runtime || "/tmp") + "/omarchy-whatsapp/daemon.sock"
  }
  readonly property string qrPath: {
    var runtime = Quickshell.env("XDG_RUNTIME_DIR")
    return String(runtime || "/tmp") + "/omarchy-whatsapp/pairing.svg"
  }
  readonly property string statePath: {
    var state = Quickshell.env("XDG_STATE_HOME")
    if (state) return String(state) + "/omarchy-whatsapp"
    return String(Quickshell.env("HOME") || "") + "/.local/state/omarchy-whatsapp"
  }
  readonly property string uiPreferencesPath: statePath + "/ui-preferences.json"
  readonly property int protocolVersion: 27

  property bool connected: false
  property bool protocolCompatible: false
  property string connectionState: "starting"
  property string connectionDetail: ""
  property double pairingExpiresAt: 0
  property int unreadTotal: 0
  property var avatarAvailable: ({})
  property var avatarRevisions: ({})
  property int mediaRevision: 0
  property var mediaOverrides: ({})
  property var mediaOverrideRevisions: ({})
  property var mediaDownloadRequests: ({})
  property var mediaDownloadRequestIds: ({})
  property int daemonGeneration: 0
  property var resourceSequences: ({})
  property var requestCommands: ({})
  property var requestDeadlines: ({})
  property int nextDeliverySerial: 0
  property var textMessageRequests: ({})
  property var textOutboxEntries: []
  property var chats: []
  property var groupParticipants: []
  property string groupParticipantsChatJid: ""
  property string groupParticipantsError: ""
  property var groupParticipantRequestJids: ({})
  property var messages: []
  property string messagesChatJid: ""
  property string messagesFirstUnreadId: ""
  property bool messagesResponseHasFollowup: false
  property int messagesResponseSerial: 0
  property int messagesNavigationSerial: 0
  property var messagesRequestIds: ({})
  property var messagesRequestJids: ({})
  property var messagesQueuedRequests: ({})
  property int messageSentSerial: 0
  property int incomingMessageSerial: 0
  property var contactPresence: ({})
  property var chatStates: ({})
  property double presenceClock: Date.now() / 1000
  property string localChatStateJid: ""
  property string localChatState: ""
  property int voiceRecordingSerial: 0
  property int voiceMessageRequestId: 0
  property string voiceMessageRequestRecordingId: ""
  property string voiceMessageRequestChatJid: ""
  property int voiceMessageRequestDurationMs: 0
  property var voiceOutboxEntries: []
  property int sentPresenceState: -1
  property var pollVoteRequests: ({})
  property int pollCreateRequestId: 0
  property string selectedChatJid: ""
  property bool panelVisible: false
  property bool panelFocused: false
  property int nextRequestId: 1
  property int reconnectAttempt: 0
  property bool launcherSyncPending: false
  property string lastError: ""
  property string lastErrorRequestId: ""
  property string chatStateResyncStatus: "idle"
  property string chatStateResyncMessage: ""
  property int chatStateResyncRequestId: 0
  readonly property bool chatStateResyncBusy:
    chatStateResyncStatus === "requested" || chatStateResyncStatus === "syncing"
  property bool daemonRuntimeChecked: false
  property bool daemonRuntimeReady: false
  property bool daemonSetupBusy: false
  property string daemonSetupDetail: ""
  property string daemonSetupError: ""
  property bool unreadOnly: false
  property bool uiPreferencesReady: false
  property bool uiPreferencesDirty: false
  property int uiPreferencesRevision: 0
  property int uiPreferencesSavingRevision: 0
  readonly property bool daemonSetupRequired: daemonRuntimeChecked
    && !daemonRuntimeReady

  signal messagesWillChange(bool preservePosition)
  signal textMessageAccepted(string deliveryId, string chatJid, string text)

  readonly property string qrImageUrl: pairingExpiresAt > 0
    ? "file://" + qrPath + "?v=" + pairingExpiresAt : ""
  readonly property var selectedChat: {
    for (var i = 0; i < chats.length; i++)
      if (String(chats[i].jid || "") === selectedChatJid) return chats[i]
    return null
  }

  function copyArray(value) {
    return Array.isArray(value) ? value.slice() : []
  }

  function clearPresenceState() {
    contactPresence = ({})
    chatStates = ({})
    localChatStateJid = ""
    localChatState = ""
    if (voiceMessageRequestId > 0)
      setLocalVoiceOutboxEntry({
        recording_id: voiceMessageRequestRecordingId,
        chat_jid: voiceMessageRequestChatJid,
        duration_ms: voiceMessageRequestDurationMs,
        status: "failed",
        error: "Connection lost; retry is safe",
        local_only: true,
        created_at: Date.now() / 1000
      })
    voiceMessageRequestId = 0
    voiceMessageRequestRecordingId = ""
    voiceMessageRequestChatJid = ""
    voiceMessageRequestDurationMs = 0
    if (typeof localTypingPauseTimer !== "undefined")
      localTypingPauseTimer.stop()
    if (typeof localRecordingRefreshTimer !== "undefined")
      localRecordingRefreshTimer.stop()
  }

  function applyPresence(frame) {
    var jid = String(frame ? frame.jid || "" : "")
    if (!jid) return false
    var lastSeen = Number(frame.last_seen || 0)
    var values = Object.assign({}, contactPresence)
    values[jid] = {
      available: frame.available === true,
      last_seen: isFinite(lastSeen) && lastSeen > 0 ? lastSeen : 0
    }
    contactPresence = values
    return true
  }

  function chatStateKey(chatJid, senderJid) {
    return String(chatJid || "") + "\n" + String(senderJid || "")
  }

  function applyChatState(frame) {
    var chatJid = String(frame ? frame.chat_jid || "" : "")
    var senderJid = String(frame ? frame.sender_jid || "" : "")
    var state = String(frame ? frame.state || "" : "")
    if (!chatJid || !senderJid
        || ["typing", "recording", "paused"].indexOf(state) < 0) return false
    var values = Object.assign({}, chatStates)
    var key = chatStateKey(chatJid, senderJid)
    if (state === "paused") {
      if (values[key] === undefined) return false
      delete values[key]
    } else {
      values[key] = {
        chat_jid: chatJid,
        sender_jid: senderJid,
        sender_name: String(frame.sender_name || ""),
        state: state,
        expires_at: presenceClock + 10
      }
    }
    chatStates = values
    return true
  }

  function clearChatState(chatJid, senderJid) {
    var key = chatStateKey(chatJid, senderJid)
    if (chatStates[key] === undefined) return false
    var values = Object.assign({}, chatStates)
    delete values[key]
    chatStates = values
    return true
  }

  function expireChatStates(nowSeconds) {
    var now = Number(nowSeconds || presenceClock)
    var values = Object.assign({}, chatStates)
    var changed = false
    for (var key in values) {
      if (Number((values[key] || {}).expires_at || 0) > now) continue
      delete values[key]
      changed = true
    }
    if (changed) chatStates = values
    return changed
  }

  function chatStateLabel(chatJid, isGroup) {
    var jid = String(chatJid || "")
    if (!jid) return ""
    var entries = []
    for (var key in chatStates) {
      var entry = chatStates[key] || {}
      if (String(entry.chat_jid || "") === jid
          && Number(entry.expires_at || 0) > presenceClock) entries.push(entry)
    }
    if (!entries.length) return ""
    entries.sort(function(left, right) {
      return String(left.sender_name || left.sender_jid || "")
        .localeCompare(String(right.sender_name || right.sender_jid || ""))
    })
    var typingNames = []
    var recordingNames = []
    for (var i = 0; i < entries.length; i++) {
      var name = Model.friendlyName(entries[i].sender_name, entries[i].sender_jid)
      if (entries[i].state === "recording") recordingNames.push(name)
      else typingNames.push(name)
    }
    if (isGroup !== true)
      return recordingNames.length ? "recording audio…" : "typing…"
    function groupAction(names, action) {
      if (names.length === 1) return names[0] + " is " + action + "…"
      if (names.length === 2)
        return names[0] + " and " + names[1] + " are " + action + "…"
      return names[0] + ", " + names[1] + " + " + (names.length - 2)
        + " are " + action + "…"
    }
    if (recordingNames.length && typingNames.length)
      return groupAction(recordingNames, "recording audio") + "; "
        + groupAction(typingNames, "typing")
    var names = recordingNames.length ? recordingNames : typingNames
    var action = recordingNames.length ? "recording audio" : "typing"
    return groupAction(names, action)
  }

  function presenceLabel(jid, nowSeconds, format, locale) {
    var value = contactPresence[String(jid || "")] || {}
    if (value.available === true) return "online"
    return Model.lastSeenLabel(value.last_seen, nowSeconds, format, locale)
  }

  function sendPresence(force) {
    var available = panelVisible && panelFocused
    var encoded = available ? 1 : 0
    if (force !== true && sentPresenceState === encoded) return false
    if (!send("set_presence", { available: available })) return false
    sentPresenceState = encoded
    return true
  }

  function pauseComposing() {
    if (!localChatStateJid || !localChatState) return false
    var jid = localChatStateJid
    localChatStateJid = ""
    localChatState = ""
    localTypingPauseTimer.stop()
    localRecordingRefreshTimer.stop()
    send("set_chat_state", { chat_jid: jid, state: "paused" })
    return true
  }

  function noteComposerActivity(text) {
    var active = String(text || "") !== "" && panelVisible && panelFocused
      && selectedChatJid && connectionState === "connected"
    if (!active) {
      pauseComposing()
      return false
    }
    if (localChatStateJid !== selectedChatJid || localChatState !== "typing") {
      pauseComposing()
      if (!send("set_chat_state", {
        chat_jid: selectedChatJid,
        state: "typing"
      })) return false
      localChatStateJid = selectedChatJid
      localChatState = "typing"
    }
    localTypingPauseTimer.restart()
    return true
  }

  function recordingIdIsValid(recordingId) {
    var value = String(recordingId || "")
    return value.length > 0 && value.length <= 80
      && /^[A-Za-z0-9](?:[A-Za-z0-9-]*[A-Za-z0-9])?$/.test(value)
  }

  function voiceOutboxForChat(chatJid) {
    var value = String(chatJid || "")
    for (var i = voiceOutboxEntries.length - 1; i >= 0; i--)
      if (String(voiceOutboxEntries[i].chat_jid || "") === value)
        return voiceOutboxEntries[i]
    return null
  }

  function setLocalVoiceOutboxEntry(entry) {
    if (!entry || !recordingIdIsValid(entry.recording_id)) return false
    var values = voiceOutboxEntries.slice()
    var replacement = Object.assign({}, entry)
    for (var i = 0; i < values.length; i++) {
      if (String(values[i].recording_id || "") !== replacement.recording_id)
        continue
      values[i] = replacement
      voiceOutboxEntries = values
      return true
    }
    values.push(replacement)
    voiceOutboxEntries = values
    return true
  }

  function removeLocalVoiceOutboxEntry(recordingId) {
    var value = String(recordingId || "")
    var filtered = voiceOutboxEntries.filter(function(entry) {
      return String(entry.recording_id || "") !== value
    })
    var changed = filtered.length !== voiceOutboxEntries.length
    if (changed) voiceOutboxEntries = filtered
    return changed
  }

  function applyVoiceOutbox(frame) {
    var input = copyArray(frame ? frame.entries : [])
    var entries = []
    for (var i = 0; i < input.length; i++) {
      var entry = input[i] || {}
      var recordingId = String(entry.recording_id || "")
      var status = String(entry.status || "")
      var duration = Math.floor(Number(entry.duration_ms || 0))
      if (!recordingIdIsValid(recordingId)
          || (status !== "sending" && status !== "failed")
          || !isFinite(duration) || duration < 250) continue
      entries.push({
        recording_id: recordingId,
        chat_jid: String(entry.chat_jid || ""),
        duration_ms: duration,
        status: status,
        error: String(entry.error || ""),
        local_only: false,
        created_at: Number(entry.created_at || 0)
      })
    }
    for (var localIndex = 0; localIndex < voiceOutboxEntries.length; localIndex++) {
      var local = voiceOutboxEntries[localIndex] || {}
      if (local.local_only !== true) continue
      var found = entries.some(function(entry) {
        return entry.recording_id === String(local.recording_id || "")
      })
      if (!found) entries.push(Object.assign({}, local))
    }
    voiceOutboxEntries = entries
    return entries.length
  }

  function newVoiceRecording() {
    if (!selectedChatJid || connectionState !== "connected") return null
    voiceRecordingSerial++
    var recordingId = String(Date.now()) + "-" + String(voiceRecordingSerial)
    return {
      recording_id: recordingId,
      chat_jid: selectedChatJid,
      path: statePath + "/outbox/voice-" + recordingId + ".ogg"
    }
  }

  function beginVoiceRecording() {
    if (!selectedChatJid || connectionState !== "connected" || !panelVisible
        || !panelFocused || voiceMessageRequestId > 0) return false
    pauseComposing()
    if (!send("set_chat_state", {
      chat_jid: selectedChatJid,
      state: "recording"
    })) return false
    localChatStateJid = selectedChatJid
    localChatState = "recording"
    localRecordingRefreshTimer.start()
    return true
  }

  function finishVoiceRecording() {
    return pauseComposing()
  }

  function sendVoiceMessage(recordingId, chatJid, durationMs) {
    var value = String(recordingId || "")
    var chat = String(chatJid || "")
    var duration = Math.floor(Number(durationMs || 0))
    if (!recordingIdIsValid(value) || !chat || voiceMessageRequestId > 0
        || !isFinite(duration)
        || duration < 250 || duration > 15 * 60 * 1000) return false
    voiceMessageRequestId = send("send_voice_message", {
      chat_jid: chat,
      recording_id: value
    })
    if (!voiceMessageRequestId) {
      setLocalVoiceOutboxEntry({
        recording_id: value,
        chat_jid: chat,
        duration_ms: duration,
        status: "failed",
        error: "Daemon is unavailable; retry is safe",
        local_only: true,
        created_at: Date.now() / 1000
      })
      return false
    }
    voiceMessageRequestRecordingId = value
    voiceMessageRequestChatJid = chat
    voiceMessageRequestDurationMs = duration
    setLocalVoiceOutboxEntry({
      recording_id: value,
      chat_jid: chat,
      duration_ms: duration,
      status: "sending",
      error: "",
      local_only: false,
      created_at: Date.now() / 1000
    })
    return true
  }

  function retryVoiceMessage(entry) {
    if (!entry) return false
    return sendVoiceMessage(entry.recording_id, entry.chat_jid, entry.duration_ms)
  }

  function discardVoiceRecording(recordingId) {
    var value = String(recordingId || "")
    if (!recordingIdIsValid(value)) return false
    if (!send("discard_voice_recording", { recording_id: value })) return false
    removeLocalVoiceOutboxEntry(value)
    return true
  }

  function finishVoiceMessageRequest(frame) {
    if (!frame || Number(frame.id || 0) !== voiceMessageRequestId) return false
    var recordingId = voiceMessageRequestRecordingId
    if (frame.event === "error") {
      setLocalVoiceOutboxEntry({
        recording_id: recordingId,
        chat_jid: voiceMessageRequestChatJid,
        duration_ms: voiceMessageRequestDurationMs,
        status: "failed",
        error: String(frame.message || "Could not send voice message"),
        local_only: true,
        created_at: Date.now() / 1000
      })
    } else if (frame.event === "sent") {
      removeLocalVoiceOutboxEntry(recordingId)
    }
    voiceMessageRequestId = 0
    voiceMessageRequestRecordingId = ""
    voiceMessageRequestChatJid = ""
    voiceMessageRequestDurationMs = 0
    return true
  function updateDaemonSetupDetail(line) {
    var detail = String(line || "").trim()
    if (!detail) return
    if (detail.indexOf("setup: ") === 0) detail = detail.substring(7)
    daemonSetupDetail = detail.length > 240
      ? detail.substring(0, 237) + "…" : detail
  }

  function checkDaemonRuntime() {
    if (runtimeCheck.running) return
    if (!daemonSetupScript) {
      daemonStarter.running = true
      return
    }
    runtimeCheck.command = ["/usr/bin/bash", daemonSetupScript, "check"]
    runtimeCheck.running = true
  }

  function setupDaemonRuntime() {
    if (daemonSetupBusy) return false
    if (!daemonSetupScript) {
      daemonRuntimeChecked = true
      daemonRuntimeReady = false
      daemonSetupError = "The plugin checkout does not include its daemon setup helper."
      return false
    }
    daemonSetupError = ""
    daemonSetupDetail = "Preparing the local build…"
    daemonSetupBusy = true
    runtimeSetup.command = ["/usr/bin/bash", daemonSetupScript, "setup"]
    runtimeSetup.running = true
    return true
  }

  function retryDaemon() {
    daemonSetupError = ""
    reconnectAttempt = 0
    if (daemonSetupRequired) {
      checkDaemonRuntime()
      return
    }
    if (!daemonStarter.running) daemonStarter.running = true
    socketLoader.active = false
    socketLoader.active = true
  }

  function groupParticipantRequestPending(jid) {
    var value = String(jid || "")
    for (var requestId in groupParticipantRequestJids)
      if (String(groupParticipantRequestJids[requestId] || "") === value)
        return true
    return false
  }

  function requestGroupParticipants(jid) {
    var value = String(jid || "")
    if (!value || groupParticipantRequestPending(value)) return false
    var requestId = send("get_group_participants", { chat_jid: value })
    if (!requestId) return false
    var requestJids = Object.assign({}, groupParticipantRequestJids)
    requestJids[String(requestId)] = value
    groupParticipantRequestJids = requestJids
    groupParticipants = []
    groupParticipantsChatJid = ""
    groupParticipantsError = ""
    return true
  }

  function finishGroupParticipantsRequest(frame) {
    if (!frame || frame.id === undefined || frame.id === null) return ""
    var requestId = String(frame.id)
    var jid = String(groupParticipantRequestJids[requestId] || "")
    if (!jid) return ""
    var requestJids = Object.assign({}, groupParticipantRequestJids)
    delete requestJids[requestId]
    groupParticipantRequestJids = requestJids
    return jid
  }

  function refreshSelectedGroupParticipants() {
    if (!selectedChat || selectedChat.is_group !== true) {
      groupParticipants = []
      groupParticipantsChatJid = ""
      groupParticipantsError = ""
      return false
    }
    return requestGroupParticipants(selectedChatJid)
  }

  function loadUiPreferences(raw) {
    if (uiPreferencesReady) return
    var storedUnreadOnly = false
    var content = String(raw || "").trim()
    if (content) {
      try {
        var parsed = JSON.parse(content)
        storedUnreadOnly = parsed && parsed.unread_only === true
      } catch (error) {
        console.warn("whatsapp: ignoring invalid UI preferences", error)
      }
    }
    if (!uiPreferencesDirty) unreadOnly = storedUnreadOnly
    uiPreferencesReady = true
    if (uiPreferencesDirty) uiPreferencesSaveTimer.restart()
  }

  function setUnreadOnly(value) {
    var next = value === true
    if (unreadOnly === next) return
    unreadOnly = next
    uiPreferencesDirty = true
    uiPreferencesRevision++
    if (uiPreferencesReady) uiPreferencesSaveTimer.restart()
  }

  function flushUiPreferences() {
    if (!uiPreferencesReady || !uiPreferencesDirty) return
    uiPreferencesSavingRevision = uiPreferencesRevision
    uiPreferencesFile.setText(JSON.stringify({
      version: 1,
      unread_only: unreadOnly
    }, null, 2) + "\n")
  }

  function normalizeMessages(value) {
    var output = copyArray(value)
    for (var i = 0; i < output.length; i++) {
      var source = output[i] || {}
      var receipt = Math.floor(Number(source.receipt || 0))
      source.receipt = isFinite(receipt) ? Math.max(0, Math.min(4, receipt)) : 0
      var deliveredAt = Math.floor(Number(source.delivered_at || 0))
      source.delivered_at = isFinite(deliveredAt) && deliveredAt > 0
        ? deliveredAt : 0
      var readAt = Math.floor(Number(source.read_at || 0))
      source.read_at = isFinite(readAt) && readAt > 0 ? readAt : 0
      var deliveries = copyArray(source.delivered_to)
      var normalizedDeliveries = []
      var seenDeliveries = ({})
      for (var deliveryIndex = 0;
          deliveryIndex < deliveries.length; deliveryIndex++) {
        var delivery = deliveries[deliveryIndex] || {}
        var deliveryJid = String(delivery.jid || "")
        if (!deliveryJid || seenDeliveries[deliveryJid] === true) continue
        seenDeliveries[deliveryJid] = true
        var recipientDeliveredAt = Math.floor(Number(delivery.delivered_at || 0))
        normalizedDeliveries.push({
          jid: deliveryJid,
          name: String(delivery.name || ""),
          delivered_at: isFinite(recipientDeliveredAt) && recipientDeliveredAt > 0
            ? recipientDeliveredAt : 0
        })
      }
      source.delivered_to = normalizedDeliveries
      var readers = copyArray(source.read_by)
      var normalizedReaders = []
      var seenReaders = ({})
      for (var readerIndex = 0; readerIndex < readers.length; readerIndex++) {
        var reader = readers[readerIndex] || {}
        var readerJid = String(reader.jid || "")
        if (!readerJid || seenReaders[readerJid] === true) continue
        seenReaders[readerJid] = true
        var readerReadAt = Math.floor(Number(reader.read_at || 0))
        normalizedReaders.push({
          jid: readerJid,
          name: String(reader.name || ""),
          read_at: isFinite(readerReadAt) && readerReadAt > 0
            ? readerReadAt : 0
        })
      }
      source.read_by = normalizedReaders
      var reactions = source.reactions
      var normalized = []
      if (reactions && typeof reactions.length === "number")
        for (var j = 0; j < reactions.length; j++) normalized.push(reactions[j])
      source.reactions = normalized
      output[i] = source
    }
    return output
  }

  function replaceMessages(value, preservePosition) {
    messagesWillChange(preservePosition === true)
    messages = copyArray(value)
  }

  function applyReceipts(frame) {
    var nextReceipt = Math.floor(Number(frame ? frame.receipt || 0 : 0))
    if (!isFinite(nextReceipt) || nextReceipt < 1) return false
    nextReceipt = Math.min(4, nextReceipt)
    var messageIds = copyArray(frame.message_ids)
    var wanted = ({})
    for (var i = 0; i < messageIds.length; i++)
      wanted[String(messageIds[i] || "")] = true
    var updated = messages.slice()
    var changed = false
    var eventTimestamp = Math.floor(Number(frame ? frame.timestamp || 0 : 0))
    if (!isFinite(eventTimestamp) || eventTimestamp <= 0) eventTimestamp = 0
    var eventDelivery = frame && frame.delivery ? frame.delivery : null
    var eventDeliveryJid = String(eventDelivery ? eventDelivery.jid || "" : "")
    var eventReader = frame && frame.reader ? frame.reader : null
    var eventReaderJid = String(eventReader ? eventReader.jid || "" : "")
    for (var messageIndex = 0; messageIndex < updated.length; messageIndex++) {
      var message = updated[messageIndex] || {}
      if (message.from_me !== true || wanted[String(message.id || "")] !== true)
        continue
      var replacement = null
      if (Number(message.receipt || 0) < nextReceipt) {
        replacement = Object.assign({}, message)
        replacement.receipt = nextReceipt
      }
      var deliveredAt = Math.floor(Number(message.delivered_at || 0))
      if (nextReceipt === 2 && eventTimestamp
          && (!deliveredAt || eventTimestamp < deliveredAt)) {
        if (!replacement) replacement = Object.assign({}, message)
        replacement.delivered_at = eventTimestamp
      }
      var readAt = Math.floor(Number(message.read_at || 0))
      if (nextReceipt >= 3 && eventTimestamp
          && (!readAt || eventTimestamp < readAt)) {
        if (!replacement) replacement = Object.assign({}, message)
        replacement.read_at = eventTimestamp
      }
      if (nextReceipt === 2 && eventDeliveryJid) {
        var deliveredTo = copyArray(message.delivered_to)
        var existingDelivery = -1
        for (var deliveryIndex = 0;
            deliveryIndex < deliveredTo.length; deliveryIndex++) {
          if (String((deliveredTo[deliveryIndex] || {}).jid || "")
              === eventDeliveryJid) {
            existingDelivery = deliveryIndex
            break
          }
        }
        var nextDeliveredAt = Math.floor(Number(
          eventDelivery.delivered_at || eventTimestamp || 0))
        if (!isFinite(nextDeliveredAt) || nextDeliveredAt <= 0)
          nextDeliveredAt = 0
        var nextDelivery = {
          jid: eventDeliveryJid,
          name: String(eventDelivery.name || ""),
          delivered_at: nextDeliveredAt
        }
        if (existingDelivery < 0) {
          deliveredTo.push(nextDelivery)
          if (!replacement) replacement = Object.assign({}, message)
          replacement.delivered_to = deliveredTo
        } else {
          var currentDelivery = deliveredTo[existingDelivery] || {}
          var currentDeliveredAt = Math.floor(Number(
            currentDelivery.delivered_at || 0))
          var deliveryNameChanged = nextDelivery.name
            && String(currentDelivery.name || "") !== nextDelivery.name
          var deliveryTimeChanged = nextDelivery.delivered_at
            && (!currentDeliveredAt
              || nextDelivery.delivered_at < currentDeliveredAt)
          if (deliveryNameChanged || deliveryTimeChanged) {
            deliveredTo[existingDelivery] = {
              jid: eventDeliveryJid,
              name: deliveryNameChanged ? nextDelivery.name
                : String(currentDelivery.name || ""),
              delivered_at: deliveryTimeChanged
                ? nextDelivery.delivered_at : currentDeliveredAt
            }
            if (!replacement) replacement = Object.assign({}, message)
            replacement.delivered_to = deliveredTo
          }
        }
      }
      if (nextReceipt >= 3 && eventReaderJid) {
        var readBy = copyArray(message.read_by)
        var existingReader = -1
        for (var readIndex = 0; readIndex < readBy.length; readIndex++)
          if (String((readBy[readIndex] || {}).jid || "") === eventReaderJid) {
            existingReader = readIndex
            break
          }
        var nextReaderAt = Math.floor(Number(
          eventReader.read_at || eventTimestamp || 0))
        if (!isFinite(nextReaderAt) || nextReaderAt <= 0) nextReaderAt = 0
        var nextReader = {
          jid: eventReaderJid,
          name: String(eventReader.name || ""),
          read_at: nextReaderAt
        }
        if (existingReader < 0) {
          readBy.push(nextReader)
          if (!replacement) replacement = Object.assign({}, message)
          replacement.read_by = readBy
        } else {
          var currentReader = readBy[existingReader] || {}
          var currentReaderAt = Math.floor(Number(currentReader.read_at || 0))
          var readerNameChanged = nextReader.name
            && String(currentReader.name || "") !== nextReader.name
          var readerTimeChanged = nextReader.read_at
            && (!currentReaderAt || nextReader.read_at < currentReaderAt)
          if (readerNameChanged || readerTimeChanged) {
            readBy[existingReader] = {
              jid: eventReaderJid,
              name: readerNameChanged ? nextReader.name
                : String(currentReader.name || ""),
              read_at: readerTimeChanged ? nextReader.read_at : currentReaderAt
            }
            if (!replacement) replacement = Object.assign({}, message)
            replacement.read_by = readBy
          }
        }
      }
      if (!replacement) continue
      updated[messageIndex] = replacement
      changed = true
    }
    if (changed) replaceMessages(updated, true)
    return changed
  }

  function hexKey(value) {
    var input = unescape(encodeURIComponent(String(value || "")))
    var output = ""
    for (var i = 0; i < input.length; i++)
      output += ("0" + input.charCodeAt(i).toString(16)).slice(-2)
    return output
  }

  function avatarUrl(jid) {
    if (!jid || String(jid) === "me") return ""
    var key = String(jid)
    if (avatarAvailable[key] !== true) return ""
    return "file://" + statePath + "/avatars/" + hexKey(key)
      + ".img?v=" + Number(avatarRevisions[key] || 0)
  }

  function fileUrl(path, revision) {
    var version = revision === undefined ? mediaRevision : revision
    return path ? "file://" + String(path) + "?v=" + version : ""
  }

  function requestAvatar(jid) {
    if (jid && String(jid) !== "me")
      send("request_avatar", { jid: String(jid) })
  }

  function openMap(latitudeE7, longitudeE7) {
    var latitude = Number(latitudeE7 || 0) / 10000000
    var longitude = Number(longitudeE7 || 0) / 10000000
    if (!isFinite(latitude) || !isFinite(longitude)) return
    mapOpener.command = ["xdg-open", "https://www.openstreetmap.org/?mlat="
      + latitude + "&mlon=" + longitude + "#map=16/" + latitude + "/" + longitude]
    mapOpener.running = true
  }

  function openFile(path) {
    var source = String(path || "")
    if (!source) return
    Quickshell.execDetached([
      "/usr/bin/bash", "-c",
      "for _ in {1..75}; do "
        + "if [ -s \"$1\" ]; then exec xdg-open \"$1\"; fi; "
        + "sleep 0.2; done; "
        + "omarchy-notification-send -g 󰈙 -t 3000 "
        + "\"Document unavailable\" \"The download has not completed.\"",
      "bash", source
    ])
  }

  function saveFile(path, fileName) {
    var source = String(path || "")
    if (!source) return
    Quickshell.execDetached([
      "/usr/bin/bash", "-c",
      "for _ in {1..75}; do [ -s \"$1\" ] && break; sleep 0.2; done; "
        + "if [ ! -s \"$1\" ]; then "
        + "omarchy-notification-send -g 󰈙 -t 3000 "
        + "\"Document unavailable\" \"The download has not completed.\"; exit 1; fi; "
        + "destination=$(xdg-user-dir DOWNLOAD 2>/dev/null || true); "
        + "[ -n \"$destination\" ] || destination=\"$HOME/Downloads\"; "
        + "mkdir -p -- \"$destination\" || exit 1; "
        + "safe=${2##*/}; [ -n \"$safe\" ] || safe=Document; "
        + "stem=$safe; suffix=; case $safe in *.*) "
        + "stem=${safe%.*}; suffix=.${safe##*.};; esac; "
        + "target=\"$destination/$safe\"; counter=1; "
        + "while [ -e \"$target\" ]; do "
        + "target=\"$destination/$stem ($counter)$suffix\"; counter=$((counter + 1)); done; "
        + "install -m 600 -- \"$1\" \"$target\" && "
        + "omarchy-notification-send -g 󰈙 -t 4000 "
        + "\"Document saved\" \"$target\"",
      "bash", source, String(fileName || "Document")
    ])
  }

  function send(command, fields) {
    var socket = socketLoader.item
    if (!socket || !socket.connected || !protocolCompatible) return 0
    var payload = { id: nextRequestId++, command: String(command || "") }
    fields = fields || {}
    for (var key in fields) payload[key] = fields[key]
    socket.write(JSON.stringify(payload) + "\n")
    socket.flush()
    var commands = Object.assign({}, requestCommands)
    var deadlines = Object.assign({}, requestDeadlines)
    commands[String(payload.id)] = payload.command
    deadlines[String(payload.id)] = Date.now() + commandTimeoutMs(payload.command)
    requestCommands = commands
    requestDeadlines = deadlines
    return payload.id
  }

  function commandTimeoutMs(command) {
    var value = String(command || "")
    if (value === "send_voice_message") return 120000
    if (["resync_chat_state", "logout"].indexOf(value) >= 0) return 60000
    return 30000
  }

  function finishRequest(frame) {
    if (!frame || frame.id === undefined || frame.id === null) return ""
    var id = String(frame.id)
    var command = String(requestCommands[id] || "")
    if (!command) return ""
    var commands = Object.assign({}, requestCommands)
    var deadlines = Object.assign({}, requestDeadlines)
    delete commands[id]
    delete deadlines[id]
    requestCommands = commands
    requestDeadlines = deadlines
    if (frame.event !== "error" && lastErrorRequestId === id) {
      lastError = ""
      lastErrorRequestId = ""
    }
    return command
  }

  function resetRequestState(reason) {
    var detail = String(reason || "Connection lost; retry is safe")
    for (var requestId in textMessageRequests) {
      if (requestCommands[requestId] === "send_message") {
        lastError = detail
        lastErrorRequestId = String(requestId)
        break
      }
    }
    requestCommands = ({})
    requestDeadlines = ({})
    textMessageRequests = ({})
    groupParticipantRequestJids = ({})
    messagesRequestIds = ({})
    messagesRequestJids = ({})
    messagesQueuedRequests = ({})
    mediaDownloadRequests = ({})
    mediaDownloadRequestIds = ({})
    pollVoteRequests = ({})
    pollCreateRequestId = 0
    chatStateResyncRequestId = 0
  }

  function expireRequests(nowMs) {
    var now = Number(nowMs || Date.now())
    var expired = []
    for (var requestId in requestDeadlines)
      if (Number(requestDeadlines[requestId] || 0) <= now) expired.push(requestId)
    for (var i = 0; i < expired.length; i++) {
      var id = expired[i]
      var command = String(requestCommands[id] || "request")
      finishGroupParticipantsRequest({ id: Number(id), event: "error" })
      finishPollRequest({ id: Number(id), event: "error" })
      finishMediaDownloadRequest({ id: Number(id), event: "error" })
      finishVoiceMessageRequest({
        id: Number(id), event: "error", message: "WhatsApp request timed out"
      })
      finishChatStateResyncRequest({
        id: Number(id), event: "error", message: "WhatsApp request timed out"
      })
      finishMessagesRequest({ id: Number(id), event: "error" })
      finishTextMessageRequest({ id: Number(id), event: "error" })
      finishRequest({ id: Number(id), event: "error" })
      lastError = "WhatsApp " + command.replace(/_/g, " ") + " timed out"
      lastErrorRequestId = id
    }
    return expired.length
  }

  function resourceKey(frame) {
    if (!frame) return ""
    var event = String(frame.event || "")
    if (event === "state") return "state"
    if (event === "chats") return "chats"
    if (event === "messages" || event === "group_participants")
      return event + ":" + String(frame.chat_jid || "")
    if (event === "message" || event === "sent")
      return "messages:" + String((frame.message || {}).chat_jid || "")
    if (event === "unread") return "unread"
    if (event === "avatars") return "avatars"
    if (event === "text_outbox") return "text_outbox"
    if (event === "voice_outbox") return "voice_outbox"
    if (event === "text_delivery")
      return "text_delivery:" + String(frame.delivery_id || "")
    if (event === "invalidated")
      return String(frame.resource || "")
        + (frame.key ? ":" + String(frame.key) : "")
    if (event === "presence") return "presence:" + String(frame.jid || "")
    if (event === "chat_state") return "chat_state:"
      + String(frame.chat_jid || "") + ":" + String(frame.sender_jid || "")
    if (frame.id !== undefined && frame.id !== null)
      return "response:" + String(frame.id)
    return ""
  }

  function acceptFrame(frame) {
    var generation = Math.floor(Number(frame ? frame.generation || 0 : 0))
    if (generation > 0 && daemonGeneration > 0 && generation < daemonGeneration)
      return false
    if (generation > daemonGeneration) {
      daemonGeneration = generation
      resourceSequences = ({})
      resetRequestState("WhatsApp reconnected; retry is safe")
    }
    var sequence = Math.floor(Number(frame ? frame.sequence || 0 : 0))
    var key = resourceKey(frame)
    if (sequence <= 0 || !key) return true
    if (sequence <= Number(resourceSequences[key] || 0)) return false
    var sequences = Object.assign({}, resourceSequences)
    sequences[key] = sequence
    resourceSequences = sequences
    return true
  }

  function requestMessages(jid, queueIfPending) {
    var value = String(jid || "")
    if (!value) return 0
    var pendingId = Number(messagesRequestIds[value] || 0)
    if (pendingId) {
      if (queueIfPending === true) {
        var queuedRequests = Object.assign({}, messagesQueuedRequests)
        queuedRequests[value] = true
        messagesQueuedRequests = queuedRequests
      }
      return pendingId
    }
    var requestId = send("get_messages", { chat_jid: value, limit: 300 })
    if (!requestId) return 0
    var requestIds = Object.assign({}, messagesRequestIds)
    var requestJids = Object.assign({}, messagesRequestJids)
    requestIds[value] = requestId
    requestJids[String(requestId)] = value
    messagesRequestIds = requestIds
    messagesRequestJids = requestJids
    return requestId
  }

  function finishMessagesRequest(frame) {
    if (!frame || frame.id === undefined || frame.id === null) return ""
    var requestId = String(frame.id)
    var jid = String(messagesRequestJids[requestId] || "")
    if (!jid) return ""
    var requestIds = Object.assign({}, messagesRequestIds)
    var requestJids = Object.assign({}, messagesRequestJids)
    var queuedRequests = Object.assign({}, messagesQueuedRequests)
    var shouldRefresh = queuedRequests[jid] === true
    if (String(requestIds[jid] || "") === requestId) delete requestIds[jid]
    delete requestJids[requestId]
    delete queuedRequests[jid]
    messagesRequestIds = requestIds
    messagesRequestJids = requestJids
    messagesQueuedRequests = queuedRequests
    return shouldRefresh ? jid : ""
  }

  function mediaDownloadKey(message) {
    if (!message) return ""
    return String(message.chat_jid || "") + "\n" + String(message.id || "")
  }

  function messageMedia(message) {
    var key = mediaDownloadKey(message)
    var revision = Number(mediaOverrideRevisions[key] || 0)
    return revision > 0 ? mediaOverrides[key] : (message ? message.media || null : null)
  }

  function messageMediaRevision(message) {
    var key = mediaDownloadKey(message)
    return String(mediaRevision) + "-"
      + String(Number(mediaOverrideRevisions[key] || 0))
  }

  function applyDownloadedMedia(frame) {
    if (!frame || String(frame.chat_jid || "") !== selectedChatJid) return
    var messageId = String(frame.message_id || "")
    var found = false
    for (var i = 0; i < messages.length; i++) {
      if (String(messages[i].id || "") === messageId) {
        found = true
        break
      }
    }
    if (!found) return
    var key = String(frame.chat_jid || "") + "\n" + messageId
    var overrides = Object.assign({}, mediaOverrides)
    var revisions = Object.assign({}, mediaOverrideRevisions)
    overrides[key] = frame.media || null
    revisions[key] = Number(revisions[key] || 0) + 1
    mediaOverrides = overrides
    mediaOverrideRevisions = revisions
  }

  function mediaDownloading(message) {
    return mediaDownloadRequests[mediaDownloadKey(message)] === true
  }

  function downloadMedia(message) {
    if (!message || !message.chat_jid || !message.id || mediaDownloading(message))
      return false
    var kind = String(message.media ? message.media.kind || "" : "")
    if (kind === "sticker" && message.media.lottie === true) return false
    var command = kind === "image" ? "download_image"
      : (kind === "sticker" ? "download_sticker"
        : (kind === "video" ? "download_video"
          : (kind === "audio" ? "download_audio" : "")))
    if (!command) return false
    var requestId = send(command, {
      chat_jid: String(message.chat_jid),
      message_id: String(message.id)
    })
    if (!requestId) return false
    var key = mediaDownloadKey(message)
    var requests = Object.assign({}, mediaDownloadRequests)
    var requestIds = Object.assign({}, mediaDownloadRequestIds)
    requests[key] = true
    requestIds[String(requestId)] = key
    mediaDownloadRequests = requests
    mediaDownloadRequestIds = requestIds
    return true
  }

  function autoDownloadStickers(value) {
    var items = copyArray(value)
    var started = 0
    for (var i = 0; i < items.length; i++) {
      var message = items[i] || {}
      var media = message.media || null
      if (!media || media.kind !== "sticker" || media.downloaded === true
          || media.lottie === true) continue
      if (downloadMedia(message)) started++
    }
    return started
  }

  function finishMediaDownloadRequest(frame) {
    if (!frame || frame.id === undefined || frame.id === null) return
    var id = String(frame.id)
    var key = mediaDownloadRequestIds[id]
    if (!key) return
    var requestIds = Object.assign({}, mediaDownloadRequestIds)
    delete requestIds[id]
    mediaDownloadRequestIds = requestIds
    // An ack only confirms the daemon queued the download; the busy marker
    // clears when the media_downloaded or media_download_failed broadcast
    // arrives. Errors and timeouts release the marker immediately.
    if (frame.event !== "error") return
    var requests = Object.assign({}, mediaDownloadRequests)
    delete requests[key]
    mediaDownloadRequests = requests
  }

  function clearMediaDownloadState(chatJid, messageId) {
    var key = String(chatJid || "") + "\n" + String(messageId || "")
    if (mediaDownloadRequests[key] !== true) return
    var requests = Object.assign({}, mediaDownloadRequests)
    delete requests[key]
    mediaDownloadRequests = requests
  }

  function refreshMetadata() {
    send("get_state")
    send("list_chats", { limit: 500 })
    send("list_avatars")
    send("list_text_outbox")
    send("list_voice_outbox")
  }

  function refresh() {
    refreshMetadata()
    if (selectedChatJid) {
      requestMessages(selectedChatJid)
      refreshSelectedGroupParticipants()
    }
  }

  function selectChat(jid) {
    var value = String(jid || "")
    if (!value) return
    var changed = value !== selectedChatJid
    if (changed) {
      selectedChatJid = value
      groupParticipants = []
      groupParticipantsChatJid = ""
      groupParticipantsError = ""
      messagesChatJid = ""
      messagesFirstUnreadId = ""
      messagesNavigationSerial++
      replaceMessages([], false)
    }
    if (changed || messagesChatJid !== value) requestMessages(value)
    if (changed) refreshSelectedGroupParticipants()
    if (panelVisible && panelFocused)
      send("mark_read", { chat_jid: value })
    requestAvatar(value)
    updateActiveChat()
  }

  function sendMessage(text) {
    var body = String(text || "")
    if (!selectedChatJid || !body.trim()) return false
    nextDeliverySerial++
    var deliveryId = "qml-" + String(Date.now()) + "-" + String(nextDeliverySerial)
    var requestId = send("send_message", {
      chat_jid: selectedChatJid,
      text: body,
      delivery_id: deliveryId
    })
    if (!requestId) return false
    var requests = Object.assign({}, textMessageRequests)
    requests[String(requestId)] = {
      delivery_id: deliveryId,
      chat_jid: selectedChatJid,
      text: body
    }
    textMessageRequests = requests
    lastError = ""
    lastErrorRequestId = ""
    pauseComposing()
    return true
  }

  function finishTextMessageRequest(frame) {
    if (!frame || frame.id === undefined || frame.id === null) return false
    var id = String(frame.id)
    var pending = textMessageRequests[id]
    if (!pending) return false
    var requests = Object.assign({}, textMessageRequests)
    delete requests[id]
    textMessageRequests = requests
    if (frame.event === "text_accepted"
        && String(frame.delivery_id || "") === String(pending.delivery_id || "")) {
      textMessageAccepted(
        String(pending.delivery_id), String(pending.chat_jid), String(pending.text))
      return true
    }
    return false
  }

  function applyTextOutbox(frame) {
    textOutboxEntries = copyArray(frame ? frame.entries : [])
    return textOutboxEntries.length
  }

  function textOutboxForChat(chatJid) {
    var value = String(chatJid || "")
    for (var i = textOutboxEntries.length - 1; i >= 0; i--)
      if (String(textOutboxEntries[i].chat_jid || "") === value)
        if (String(textOutboxEntries[i].status || "") === "failed")
          return textOutboxEntries[i]
    return null
  }

  function retryTextMessage(entry) {
    var deliveryId = String(entry ? entry.delivery_id || "" : "")
    if (!deliveryId || String(entry.status || "") !== "failed") return false
    if (!send("retry_text_message", { delivery_id: deliveryId })) return false
    var entries = textOutboxEntries.slice()
    for (var i = 0; i < entries.length; i++) {
      if (String(entries[i].delivery_id || "") !== deliveryId) continue
      entries[i] = Object.assign({}, entries[i], { status: "queued", error: "" })
    }
    textOutboxEntries = entries
    return true
  }

  function discardTextMessage(entry) {
    var deliveryId = String(entry ? entry.delivery_id || "" : "")
    var status = String(entry ? entry.status || "" : "")
    if (!deliveryId || (status !== "queued" && status !== "failed")) return false
    if (!send("discard_text_message", { delivery_id: deliveryId })) return false
    textOutboxEntries = textOutboxEntries.filter(function(value) {
      return String(value.delivery_id || "") !== deliveryId
    })
    return true
  }

  function handleInvalidation(frame) {
    var resource = String(frame ? frame.resource || "" : "")
    var key = String(frame ? frame.key || "" : "")
    if (resource === "chats") return send("list_chats", { limit: 500 }) > 0
    if (resource === "messages")
      return key === selectedChatJid && requestMessages(key, true) > 0
    if (resource === "unread") return send("get_state") > 0
    if (resource === "avatars") return send("list_avatars") > 0
    if (resource === "text_outbox") return send("list_text_outbox") > 0
    if (resource === "voice_outbox") return send("list_voice_outbox") > 0
    return false
  }

  function setChatPinned(jid, pinned) {
    var value = String(jid || "")
    if (!value) return false
    return send("set_chat_pinned", {
      chat_jid: value,
      pinned: pinned === true
    }) > 0
  }

  function requestChatStateResync() {
    if (connectionState !== "connected" || chatStateResyncBusy) return false
    var requestId = send("resync_chat_state")
    if (!requestId) return false
    chatStateResyncRequestId = requestId
    chatStateResyncStatus = "requested"
    chatStateResyncMessage = "Chat-state resync requested"
    return true
  }

  function applyChatStateResync(frame) {
    var status = String(frame ? frame.status || "" : "")
    if (["idle", "requested", "syncing", "succeeded", "failed"]
        .indexOf(status) < 0) return false
    chatStateResyncStatus = status
    chatStateResyncMessage = String(frame.message || "")
    return true
  }

  function finishChatStateResyncRequest(frame) {
    if (!frame || frame.id === undefined || frame.id === null
        || Number(frame.id) !== chatStateResyncRequestId) return false
    chatStateResyncRequestId = 0
    if (frame.event === "error") {
      chatStateResyncStatus = "failed"
      chatStateResyncMessage = String(
        frame.message || "Could not request a WhatsApp chat-state resync")
    }
    return true
  }

  function createPoll(question, options, multipleAnswers) {
    var title = String(question || "").trim()
    var normalized = []
    var source = Array.isArray(options) ? options : []
    for (var i = 0; i < source.length; i++) {
      var option = String(source[i] || "").trim()
      if (option && normalized.indexOf(option) < 0) normalized.push(option)
    }
    if (!selectedChatJid || !title || normalized.length < 2
        || normalized.length > 12 || pollCreateRequestId > 0) return false
    pollCreateRequestId = send("create_poll", {
      chat_jid: selectedChatJid,
      question: title,
      options: normalized,
      selectable_count: multipleAnswers === true ? normalized.length : 1
    })
    return pollCreateRequestId > 0
  }

  function pollVoteKey(message) {
    return message ? String(message.chat_jid || selectedChatJid) + "\n"
      + String(message.id || "") : ""
  }

  function pollVotePending(message) {
    return pollVoteRequests[pollVoteKey(message)] !== undefined
  }

  function votePoll(message, selectedOptions) {
    if (!message || !message.id || !selectedChatJid || pollVotePending(message))
      return false
    var requestId = send("vote_poll", {
      chat_jid: selectedChatJid,
      message_id: String(message.id),
      selected_options: copyArray(selectedOptions)
    })
    if (!requestId) return false
    var requests = Object.assign({}, pollVoteRequests)
    requests[pollVoteKey(message)] = requestId
    pollVoteRequests = requests
    return true
  }

  function finishPollRequest(frame) {
    if (!frame || frame.id === undefined || frame.id === null) return
    var requestId = Number(frame.id)
    if (requestId === pollCreateRequestId) pollCreateRequestId = 0
    var requests = Object.assign({}, pollVoteRequests)
    var changed = false
    for (var key in requests) {
      if (Number(requests[key]) === requestId) {
        delete requests[key]
        changed = true
      }
    }
    if (changed) pollVoteRequests = requests
  }

  function unlinkDevice() {
    if (connectionState !== "connected") return false
    return send("logout") > 0
  }

  function reactToMessage(message, emoji) {
    if (!message || !selectedChatJid || !message.id) return false
    return send("react", {
      chat_jid: selectedChatJid,
      message_id: String(message.id),
      sender_jid: String(message.sender_jid || ""),
      target_from_me: message.from_me === true,
      emoji: String(emoji || "")
    }) > 0
  }

  function updateActiveChat() {
    send("set_active_chat", {
      chat_jid: panelVisible && panelFocused && selectedChatJid
        ? selectedChatJid : null
    })
  }

  function setPanelState(visible, focused) {
    panelVisible = visible === true
    panelFocused = focused === true
    if (!panelVisible || !panelFocused) pauseComposing()
    updateActiveChat()
    sendPresence()
  }

  function openPanel(chatJid) {
    if (!shell || typeof shell.summon !== "function") return "unavailable"
    var payload = chatJid ? { chatJid: String(chatJid) } : {}
    return shell.summon(pluginId, JSON.stringify(payload)) ? "opened" : "unavailable"
  }

  function scheduleLauncherSync() {
    if (launcherSync.running)
      launcherSyncPending = true
    else
      launcherSyncDebounce.restart()
  }

  function handleState(frame) {
    var status = frame.status || {}
    connectionState = String(status.state || "starting")
    connectionDetail = String(status.reason || status.message || "")
    pairingExpiresAt = Number(status.expires_at || 0)
    unreadTotal = Number(frame.unread_total || 0)
    if (connectionState !== "connected") {
      clearPresenceState()
      sentPresenceState = -1
    } else {
      sendPresence()
      updateActiveChat()
    }
  }

  function handleHello(frame) {
    var received = Number(frame ? frame.protocol_version : 0)
    if (!isFinite(received) || Math.floor(received) !== received
        || received !== protocolVersion) {
      protocolCompatible = false
      connectionState = "error"
      var versionLabel = isFinite(received) && received > 0
        ? String(received) : "unknown"
      connectionDetail = "WhatsApp component version mismatch (shell protocol "
        + protocolVersion + ", daemon protocol " + versionLabel
        + "). Reinstall or update the plugin."
      lastError = connectionDetail
      clearPresenceState()
      return false
    }
    protocolCompatible = true
    connectionDetail = ""
    lastError = ""
    refresh()
    updateActiveChat()
    sendPresence(true)
    return true
  }

  function handleLine(line) {
    var frame
    try { frame = JSON.parse(String(line || "")) }
    catch (error) { return }
    if (!frame || !frame.event) return
    if (frame.event === "hello") {
      if (!acceptFrame(frame)) return
      handleHello(frame)
      return
    }
    if (!protocolCompatible) return
    if (!acceptFrame(frame)) return
    var frameId = frame.id === undefined || frame.id === null
      ? "" : String(frame.id)
    var requestedMessagesJid = frameId
      ? String(messagesRequestJids[frameId] || "") : ""
    var requestedGroupParticipantsJid = finishGroupParticipantsRequest(frame)
    finishPollRequest(frame)
    finishMediaDownloadRequest(frame)
    finishVoiceMessageRequest(frame)
    finishChatStateResyncRequest(frame)
    var queuedMessagesJid = finishMessagesRequest(frame)
    finishTextMessageRequest(frame)
    if (frame.event === "state") {
      handleState(frame)
    } else if (frame.event === "chats") {
      chats = copyArray(frame.chats)
      scheduleLauncherSync()
      if (!selectedChatJid && chats.length && panelVisible)
        selectChat(chats[0].jid)
      else if (selectedChatJid && groupParticipantsChatJid !== selectedChatJid
          && !groupParticipantRequestPending(selectedChatJid))
        refreshSelectedGroupParticipants()
    } else if (frame.event === "group_participants") {
      if (String(frame.chat_jid || "") === selectedChatJid) {
        groupParticipants = copyArray(frame.participants)
        groupParticipantsChatJid = String(frame.chat_jid || "")
        groupParticipantsError = ""
      }
    } else if (frame.event === "media_downloaded") {
      clearMediaDownloadState(frame.chat_jid, frame.message_id)
      applyDownloadedMedia(frame)
    } else if (frame.event === "media_download_failed") {
      clearMediaDownloadState(frame.chat_jid, frame.message_id)
      lastError = String(frame.message || "WhatsApp media download failed")
      lastErrorRequestId = ""
    } else if (frame.event === "receipts") {
      applyReceipts(frame)
    } else if (frame.event === "presence") {
      applyPresence(frame)
    } else if (frame.event === "chat_state") {
      applyChatState(frame)
    } else if (frame.event === "chat_state_resync") {
      applyChatStateResync(frame)
    } else if (frame.event === "voice_outbox") {
      applyVoiceOutbox(frame)
    } else if (frame.event === "text_outbox") {
      applyTextOutbox(frame)
    } else if (frame.event === "text_delivery") {
      if (String(frame.status || "") === "failed") {
        lastError = String(frame.error || "WhatsApp could not deliver the message")
        lastErrorRequestId = ""
      }
      send("list_text_outbox")
    } else if (frame.event === "invalidated") {
      handleInvalidation(frame)
    } else if (frame.event === "messages") {
      if (String(frame.chat_jid || "") === selectedChatJid) {
        var preserveMessagePosition = messagesChatJid === selectedChatJid
          && messages.length > 0
        messagesResponseHasFollowup = queuedMessagesJid === selectedChatJid
        messagesChatJid = String(frame.chat_jid || "")
        messagesFirstUnreadId = String(frame.first_unread_message_id || "")
        var normalizedMessages = normalizeMessages(frame.messages)
        replaceMessages(normalizedMessages, preserveMessagePosition)
        mediaRevision++
        mediaOverrides = ({})
        mediaOverrideRevisions = ({})
        autoDownloadStickers(messages)
        messagesResponseSerial++
        var requested = {}
        for (var i = messages.length - 1; i >= 0 && i >= messages.length - 40; i--) {
          var sender = String(messages[i].sender_jid || "")
          if (sender && sender !== "me" && !requested[sender]) {
            requested[sender] = true
            requestAvatar(sender)
          }
        }
      }
    } else if (frame.event === "message" || frame.event === "sent") {
      var message = frame.message || {}
      if (String(message.chat_jid || "") === selectedChatJid) {
        if (frame.event === "sent") messageSentSerial++
        else {
          incomingMessageSerial++
          clearChatState(message.chat_jid, message.sender_jid)
        }
        requestMessages(selectedChatJid, true)
      }
    } else if (frame.event === "unread") {
      unreadTotal = Number(frame.total || 0)
    } else if (frame.event === "avatars") {
      var available = {}
      var avatarJids = copyArray(frame.jids)
      for (var avatarIndex = 0; avatarIndex < avatarJids.length; avatarIndex++)
        available[String(avatarJids[avatarIndex])] = true
      var revisions = Object.assign({}, avatarRevisions)
      var changedAvatarJids = Array.isArray(frame.changed_jids)
        ? copyArray(frame.changed_jids) : avatarJids
      var revision = Number(frame.revision || 0)
      for (var changedIndex = 0; changedIndex < changedAvatarJids.length; changedIndex++)
        revisions[String(changedAvatarJids[changedIndex])] = revision
      avatarAvailable = available
      avatarRevisions = revisions
    } else if (frame.event === "error") {
      lastError = String(frame.message || "WhatsApp command failed")
      lastErrorRequestId = frameId
      if (requestCommands[frameId] === "retry_text_message"
          || requestCommands[frameId] === "discard_text_message")
        send("list_text_outbox")
      if (requestedGroupParticipantsJid === selectedChatJid) {
        groupParticipants = []
        groupParticipantsChatJid = selectedChatJid
        groupParticipantsError = lastError
      }
    }
    finishRequest(frame)
    if (queuedMessagesJid && queuedMessagesJid === selectedChatJid)
      requestMessages(queuedMessagesJid)
  }

  function markSelectedRead() {
    if (!panelVisible || !panelFocused || !selectedChatJid) return false
    return send("mark_read", { chat_jid: selectedChatJid }) > 0
  }

  onPanelVisibleChanged: {
    updateActiveChat()
    sendPresence()
    if (panelVisible && panelFocused) markSelectedRead()
  }
  onPanelFocusedChanged: {
    if (!panelFocused) pauseComposing()
    updateActiveChat()
    sendPresence()
    if (panelFocused && panelVisible) markSelectedRead()
  }
  onSelectedChatJidChanged: {
    pauseComposing()
    updateActiveChat()
  }

  Component {
    id: socketComponent
    Socket {
      path: root.socketPath
      connected: true
      parser: SplitParser {
        splitMarker: "\n"
        onRead: function(line) { root.handleLine(line) }
      }
      onConnectionStateChanged: {
        root.connected = connected
        if (connected) {
          root.protocolCompatible = false
          root.resetRequestState("")
          root.daemonRuntimeChecked = true
          root.daemonRuntimeReady = true
          root.daemonSetupError = ""
          root.daemonSetupDetail = ""
          root.reconnectAttempt = 0
          root.sentPresenceState = -1
        } else {
          root.protocolCompatible = false
          root.resetRequestState("Connection lost; retry is safe")
          root.clearPresenceState()
          root.sentPresenceState = -1
        }
      }
      onError: function(_error) {
        root.connected = false
        root.protocolCompatible = false
        root.connectionState = "starting"
        root.resetRequestState("Connection lost; retry is safe")
        root.clearPresenceState()
        root.sentPresenceState = -1
      }
    }
  }

  Loader {
    id: socketLoader
    active: true
    sourceComponent: socketComponent
  }

  Timer {
    interval: Math.min(8000, 500 + root.reconnectAttempt * 600)
    repeat: true
    running: !root.connected
    triggeredOnStart: true
    onTriggered: {
      root.reconnectAttempt++
      if (root.reconnectAttempt === 2 && !root.daemonSetupBusy
          && (!root.daemonRuntimeChecked || root.daemonRuntimeReady))
        daemonStarter.running = true
      socketLoader.active = false
      socketLoader.active = true
    }
  }

  Timer {
    id: localTypingPauseTimer
    interval: 5000
    repeat: false
    onTriggered: root.pauseComposing()
  }

  Timer {
    id: localRecordingRefreshTimer
    interval: 8000
    repeat: true
    onTriggered: {
      if (root.localChatState !== "recording" || !root.localChatStateJid) {
        stop()
        return
      }
      root.send("set_chat_state", {
        chat_jid: root.localChatStateJid,
        state: "recording"
      })
    }
  }

  Timer {
    interval: 1000
    repeat: true
    running: Object.keys(root.chatStates).length > 0
    onTriggered: {
      root.presenceClock = Date.now() / 1000
      root.expireChatStates(root.presenceClock)
    }
  }

  Timer {
    interval: 30000
    repeat: true
    running: root.connected
    onTriggered: root.send("ping")
  }

  Timer {
    interval: 1000
    repeat: true
    running: Object.keys(root.requestDeadlines).length > 0
    onTriggered: root.expireRequests(Date.now())
  }

  Timer {
    id: launcherSyncDebounce
    interval: 1200
    onTriggered: {
      if (launcherSync.running) {
        root.launcherSyncPending = true
        return
      }
      root.launcherSyncPending = false
      launcherSync.running = true
    }
  }

  Timer {
    id: uiPreferencesSaveTimer
    interval: 150
    repeat: false
    onTriggered: root.flushUiPreferences()
  }

  FileView {
    id: uiPreferencesFile
    path: root.uiPreferencesPath
    watchChanges: false
    atomicWrites: true
    printErrors: false
    onLoaded: root.loadUiPreferences(text())
    onLoadFailed: root.loadUiPreferences("")
    onSaved: {
      if (root.uiPreferencesSavingRevision === root.uiPreferencesRevision)
        root.uiPreferencesDirty = false
      else
        uiPreferencesSaveTimer.restart()
    }
    onSaveFailed: if (!ensureUiPreferencesDir.running)
      ensureUiPreferencesDir.running = true
  }

  Process {
    id: daemonStarter
    command: ["systemctl", "--user", "start", "omarchy-whatsapp.service"]
    onExited: function(exitCode) {
      if (exitCode === 0) {
        root.daemonRuntimeChecked = true
        root.daemonRuntimeReady = true
        return
      }
      root.daemonRuntimeChecked = true
      root.daemonRuntimeReady = false
      if (!root.daemonSetupScript)
        root.daemonSetupError = "The plugin source directory is unavailable."
    }
  }

  Process {
    id: runtimeCheck
    onExited: function(exitCode) {
      if (exitCode === 0) {
        root.daemonRuntimeChecked = true
        root.daemonRuntimeReady = true
        if (!daemonStarter.running) daemonStarter.running = true
        return
      }
      if (exitCode === 127) {
        if (!daemonStarter.running) daemonStarter.running = true
        return
      }
      root.daemonRuntimeChecked = true
      root.daemonRuntimeReady = false
    }
  }

  Process {
    id: runtimeSetup
    stdout: SplitParser {
      splitMarker: "\n"
      onRead: function(line) { root.updateDaemonSetupDetail(line) }
    }
    stderr: SplitParser {
      splitMarker: "\n"
      onRead: function(line) { root.updateDaemonSetupDetail(line) }
    }
    onExited: function(exitCode) {
      root.daemonSetupBusy = false
      root.daemonRuntimeChecked = true
      root.daemonRuntimeReady = exitCode === 0
      if (exitCode === 0) {
        root.daemonSetupError = ""
        root.daemonSetupDetail = "Starting WhatsApp…"
        root.reconnectAttempt = 0
        socketLoader.active = false
        socketLoader.active = true
        return
      }
      root.daemonSetupError = exitCode === 20
        ? "WhatsApp setup needs jq, which is normally included with Omarchy."
        : (exitCode === 21
          ? "Install mise or a Rust toolchain, then try the daemon setup again."
          : (root.daemonSetupDetail || "The WhatsApp daemon could not be set up."))
    }
  }

  Process {
    id: ensureUiPreferencesDir
    command: ["/usr/bin/mkdir", "-p", root.statePath]
    onExited: {
      if (!root.uiPreferencesReady) uiPreferencesFile.reload()
      else if (root.uiPreferencesDirty) root.flushUiPreferences()
    }
  }

  Process {
    id: launcherSync
    command: ["omarchy-whatsappctl", "launcher-sync"]
    onExited: {
      if (!root.launcherSyncPending)
        return
      root.launcherSyncPending = false
      launcherSyncDebounce.restart()
    }
  }

  Process { id: mapOpener }

  IpcHandler {
    target: root.pluginId

    function open(): string { return root.openPanel("") }
    function show(): string { return root.openPanel("") }
    function openChat(chatJid: string): string { return root.openPanel(chatJid) }
    function refresh(): string { root.refresh(); return "ok" }
    function unreadFilter(): string { return root.unreadOnly ? "on" : "off" }
    function setUnreadFilter(enabled: bool): string {
      root.setUnreadOnly(enabled)
      return root.unreadOnly ? "on" : "off"
    }
  }

  Component.onCompleted: checkDaemonRuntime()
  Component.onDestruction: {
    pauseComposing()
    send("set_presence", { available: false })
    send("set_active_chat", { chat_jid: null })
  }
}
