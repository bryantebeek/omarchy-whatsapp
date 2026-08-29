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

  property bool connected: false
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
  property bool unreadOnly: false
  property bool uiPreferencesReady: false
  property bool uiPreferencesDirty: false
  property int uiPreferencesRevision: 0
  property int uiPreferencesSavingRevision: 0

  signal messagesWillChange(bool preservePosition)

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
    if (typeof localTypingPauseTimer !== "undefined")
      localTypingPauseTimer.stop()
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
    if (!socket || !socket.connected) return 0
    var payload = { id: nextRequestId++, command: String(command || "") }
    fields = fields || {}
    for (var key in fields) payload[key] = fields[key]
    socket.write(JSON.stringify(payload) + "\n")
    socket.flush()
    return payload.id
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
    var requests = Object.assign({}, mediaDownloadRequests)
    var requestIds = Object.assign({}, mediaDownloadRequestIds)
    delete requests[key]
    delete requestIds[id]
    mediaDownloadRequests = requests
    mediaDownloadRequestIds = requestIds
  }

  function refreshMetadata() {
    send("get_state")
    send("list_chats", { limit: 500 })
    send("list_avatars")
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
    send("mark_read", { chat_jid: value })
    requestAvatar(value)
    updateActiveChat()
  }

  function sendMessage(text) {
    var body = String(text || "")
    if (!selectedChatJid || !body.trim()) return false
    send("send_message", { chat_jid: selectedChatJid, text: body })
    pauseComposing()
    return true
  }

  function setChatPinned(jid, pinned) {
    var value = String(jid || "")
    if (!value) return false
    return send("set_chat_pinned", {
      chat_jid: value,
      pinned: pinned === true
    }) > 0
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
    send("react", {
      chat_jid: selectedChatJid,
      message_id: String(message.id),
      sender_jid: String(message.sender_jid || ""),
      target_from_me: message.from_me === true,
      emoji: String(emoji || "")
    })
    return true
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

  function handleLine(line) {
    var frame
    try { frame = JSON.parse(String(line || "")) }
    catch (error) { return }
    if (!frame || !frame.event) return
    var frameId = frame.id === undefined || frame.id === null
      ? "" : String(frame.id)
    var requestedMessagesJid = frameId
      ? String(messagesRequestJids[frameId] || "") : ""
    var requestedGroupParticipantsJid = finishGroupParticipantsRequest(frame)
    finishPollRequest(frame)
    finishMediaDownloadRequest(frame)
    var queuedMessagesJid = finishMessagesRequest(frame)
    lastError = ""
    if (frame.event === "hello") {
      refresh()
    } else if (frame.event === "state") {
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
      applyDownloadedMedia(frame)
    } else if (frame.event === "receipts") {
      applyReceipts(frame)
    } else if (frame.event === "presence") {
      applyPresence(frame)
    } else if (frame.event === "chat_state") {
      applyChatState(frame)
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
      send("list_chats", { limit: 500 })
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
      if (requestedGroupParticipantsJid === selectedChatJid) {
        groupParticipants = []
        groupParticipantsChatJid = selectedChatJid
        groupParticipantsError = lastError
      }
    }
    if (queuedMessagesJid && queuedMessagesJid === selectedChatJid)
      requestMessages(queuedMessagesJid)
  }

  onPanelVisibleChanged: {
    updateActiveChat()
    sendPresence()
  }
  onPanelFocusedChanged: {
    if (!panelFocused) pauseComposing()
    updateActiveChat()
    sendPresence()
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
          root.groupParticipantRequestJids = ({})
          root.messagesRequestIds = ({})
          root.messagesRequestJids = ({})
          root.messagesQueuedRequests = ({})
          root.pollVoteRequests = ({})
          root.pollCreateRequestId = 0
          root.reconnectAttempt = 0
          root.lastError = ""
          root.sentPresenceState = -1
          root.refresh()
          root.updateActiveChat()
          root.sendPresence(true)
        } else {
          root.clearPresenceState()
          root.sentPresenceState = -1
        }
      }
      onError: function(_error) {
        root.connected = false
        root.connectionState = "starting"
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
      if (root.reconnectAttempt === 2) daemonStarter.running = true
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

  Component.onCompleted: daemonStarter.running = true
  Component.onDestruction: {
    pauseComposing()
    send("set_presence", { available: false })
    send("set_active_chat", { chat_jid: null })
  }
}
