import QtQuick
import Quickshell
import Quickshell.Io

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

  property bool connected: false
  property string connectionState: "starting"
  property string connectionDetail: ""
  property double pairingExpiresAt: 0
  property int unreadTotal: 0
  property var avatarAvailable: ({})
  property var avatarRevisions: ({})
  property int mediaRevision: 0
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
  property string selectedChatJid: ""
  property bool panelVisible: false
  property bool panelFocused: false
  property int nextRequestId: 1
  property int reconnectAttempt: 0
  property bool launcherSyncPending: false
  property string lastError: ""
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
      var reactions = source.reactions
      var normalized = []
      if (reactions && typeof reactions.length === "number")
        for (var j = 0; j < reactions.length; j++) normalized.push(reactions[j])
      source.reactions = normalized
      output[i] = source
    }
    return output
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

  function fileUrl(path) {
    return path ? "file://" + String(path) + "?v=" + mediaRevision : ""
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

  function mediaDownloading(message) {
    return mediaDownloadRequests[mediaDownloadKey(message)] === true
  }

  function downloadMedia(message) {
    if (!message || !message.chat_jid || !message.id || mediaDownloading(message))
      return false
    var kind = String(message.media ? message.media.kind || "" : "")
    var command = kind === "image" ? "download_image"
      : (kind === "video" ? "download_video" : "")
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
      messages = []
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
    return true
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
    updateActiveChat()
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
    } else if (frame.event === "messages") {
      if (String(frame.chat_jid || "") === selectedChatJid) {
        messagesWillChange(requestedMessagesJid === ""
          && messagesChatJid === selectedChatJid && messages.length > 0)
        messagesResponseHasFollowup = queuedMessagesJid === selectedChatJid
        messagesChatJid = String(frame.chat_jid || "")
        messagesFirstUnreadId = String(frame.first_unread_message_id || "")
        mediaRevision++
        messages = normalizeMessages(frame.messages)
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

  onPanelVisibleChanged: updateActiveChat()
  onPanelFocusedChanged: updateActiveChat()
  onSelectedChatJidChanged: updateActiveChat()

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
          root.daemonRuntimeChecked = true
          root.daemonRuntimeReady = true
          root.daemonSetupError = ""
          root.daemonSetupDetail = ""
          root.groupParticipantRequestJids = ({})
          root.messagesRequestIds = ({})
          root.messagesRequestJids = ({})
          root.messagesQueuedRequests = ({})
          root.reconnectAttempt = 0
          root.lastError = ""
          root.refresh()
        }
      }
      onError: function(_error) {
        root.connected = false
        root.connectionState = "starting"
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
  Component.onDestruction: send("set_active_chat", { chat_jid: null })
}
