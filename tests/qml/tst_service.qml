import QtQuick
import QtTest
import Quickshell
import Quickshell.Io

import "../../quickshell" as Whatsapp

TestCase {
  id: testCase
  name: "ServiceState"

  property var service: null
  property int messagesWillChangeCount: 0
  property bool lastPreservePosition: false
  property int textAcceptedCount: 0
  property string acceptedDeliveryId: ""
  property string acceptedText: ""

  Connections {
    target: testCase.service
    function onMessagesWillChange(preservePosition) {
      testCase.messagesWillChangeCount++
      testCase.lastPreservePosition = preservePosition
    }
    function onTextMessageAccepted(deliveryId, _chatJid, text) {
      testCase.textAcceptedCount++
      testCase.acceptedDeliveryId = deliveryId
      testCase.acceptedText = text
    }
  }

  Component {
    id: serviceComponent
    Whatsapp.Service {}
  }

  function init() {
    TestIo.reset()
    Quickshell.reset()
    service = createTemporaryObject(serviceComponent, testCase, {
      manifest: { id: "test.whatsapp" }
    })
    verify(service !== null)
    tryCompare(service, "connected", true)
    service.handleLine(JSON.stringify({
      event: "hello", protocol_version: service.protocolVersion
    }))
    compare(service.protocolCompatible, true)
    TestIo.socketWrites = []
    TestIo.processStarts = []
    messagesWillChangeCount = 0
    lastPreservePosition = false
    textAcceptedCount = 0
    acceptedDeliveryId = ""
    acceptedText = ""
  }

  function cleanup() {
    if (service) service.destroy()
    service = null
  }

  function sentFrames() {
    var frames = []
    for (var i = 0; i < TestIo.socketWrites.length; i++) {
      var lines = String(TestIo.socketWrites[i]).trim().split("\n")
      for (var j = 0; j < lines.length; j++)
        if (lines[j]) frames.push(JSON.parse(lines[j]))
    }
    return frames
  }

  function lastFrame() {
    var frames = sentFrames()
    verify(frames.length > 0)
    return frames[frames.length - 1]
  }

  function test_paths_and_manifest() {
    compare(service.pluginId, "test.whatsapp")
    compare(service.socketPath, "/tmp/omarchy-whatsapp-qml-tests/runtime/omarchy-whatsapp/daemon.sock")
    compare(service.qrPath, "/tmp/omarchy-whatsapp-qml-tests/runtime/omarchy-whatsapp/pairing.svg")
    compare(service.statePath, "/tmp/omarchy-whatsapp-qml-tests/state/omarchy-whatsapp")
    compare(service.qrImageUrl, "")
    service.pairingExpiresAt = 42
    verify(service.qrImageUrl.endsWith("pairing.svg?v=42"))
  }

  function test_send_and_refresh() {
    var id = service.send("custom", { value: 7, command: "overridden" })
    verify(id > 0)
    var frame = lastFrame()
    compare(frame.id, id)
    compare(frame.command, "overridden")
    compare(frame.value, 7)

    var socket = TestIo.sockets[TestIo.sockets.length - 1]
    socket.connected = false
    compare(service.send("offline"), 0)
    socket.connected = true
    compare(service.protocolCompatible, false)
    compare(service.send("before-handshake"), 0)
    service.handleLine(JSON.stringify({
      event: "hello", protocol_version: service.protocolVersion
    }))
    TestIo.socketWrites = []

    service.refreshMetadata()
    var frames = sentFrames()
    compare(frames[frames.length - 5].command, "get_state")
    compare(frames[frames.length - 4].command, "list_chats")
    compare(frames[frames.length - 3].command, "list_avatars")
    compare(frames[frames.length - 2].command, "list_text_outbox")
    compare(frames[frames.length - 1].command, "list_voice_outbox")
  }

  function test_chat_state_resync_request_lifecycle_and_recovery() {
    service.connectionState = "disconnected"
    compare(service.requestChatStateResync(), false)
    service.connectionState = "connected"
    compare(service.requestChatStateResync(), true)
    var request = lastFrame()
    compare(request.command, "resync_chat_state")
    compare(service.chatStateResyncRequestId, request.id)
    compare(service.chatStateResyncStatus, "requested")
    compare(service.chatStateResyncBusy, true)
    compare(service.requestChatStateResync(), false)

    service.handleLine(JSON.stringify({
      event: "chat_state_resync",
      status: "syncing",
      message: "Requesting authoritative chat state from WhatsApp"
    }))
    compare(service.chatStateResyncStatus, "syncing")
    compare(service.chatStateResyncMessage,
      "Requesting authoritative chat state from WhatsApp")
    compare(service.applyChatStateResync({ status: "unexpected" }), false)
    compare(service.chatStateResyncStatus, "syncing")

    service.handleLine(JSON.stringify({
      event: "chat_state_resync",
      status: "succeeded",
      message: "WhatsApp chat state is up to date"
    }))
    compare(service.chatStateResyncStatus, "succeeded")
    compare(service.chatStateResyncBusy, false)
    compare(service.requestChatStateResync(), true)
    var failedRequest = lastFrame()
    service.handleLine(JSON.stringify({
      id: failedRequest.id,
      event: "error",
      message: "Synthetic replay failure"
    }))
    compare(service.chatStateResyncRequestId, 0)
    compare(service.chatStateResyncStatus, "failed")
    compare(service.chatStateResyncMessage, "Synthetic replay failure")
    compare(service.chatStateResyncBusy, false)

    service.handleLine(JSON.stringify({
      event: "chat_state_resync", status: "idle"
    }))
    compare(service.chatStateResyncStatus, "idle")
    compare(service.chatStateResyncMessage, "")
  }

  function test_protocol_handshake_rejects_version_skew_and_drives_resync() {
    TestIo.socketWrites = []
    service.handleLine(JSON.stringify({
      event: "hello", protocol_version: service.protocolVersion + 1
    }))
    compare(service.protocolCompatible, false)
    compare(service.connectionState, "error")
    verify(service.connectionDetail.indexOf("component version mismatch") >= 0)
    verify(service.connectionDetail.indexOf(String(service.protocolVersion + 1)) >= 0)
    compare(service.send("blocked"), 0)
    service.handleLine(JSON.stringify({
      event: "state", status: { state: "connected" }, unread_total: 99
    }))
    compare(service.connectionState, "error")
    compare(service.unreadTotal, 0)

    service.handleLine(JSON.stringify({ event: "hello" }))
    compare(service.protocolCompatible, false)
    verify(service.connectionDetail.indexOf("unknown") >= 0)

    service.handleLine(JSON.stringify({
      event: "hello", protocol_version: service.protocolVersion
    }))
    compare(service.protocolCompatible, true)
    compare(service.lastError, "")
    var frames = sentFrames()
    verify(frames.some(function(frame) { return frame.command === "get_state" }))
    verify(frames.some(function(frame) { return frame.command === "list_chats" }))
    verify(frames.some(function(frame) { return frame.command === "list_avatars" }))
    verify(frames.some(function(frame) { return frame.command === "list_voice_outbox" }))
  }

  function test_copy_normalize_and_hex_helpers() {
    var original = [
      {
        id: "one",
        receipt: 9,
        delivered_at: "123.9",
        read_at: "invalid",
        delivered_to: [
          { jid: "bob@s.whatsapp.net", name: "Bob", delivered_at: "234.9" },
          { jid: "bob@s.whatsapp.net", name: "Duplicate" },
          { name: "Missing address" }
        ],
        read_by: [
          { jid: "alice@s.whatsapp.net", name: "Alice", read_at: "456.8" },
          { jid: "alice@s.whatsapp.net", name: "Duplicate" },
          { name: "Missing address" }
        ],
        reactions: { 0: "👍", length: 1 }
      }, null
    ]
    var copy = service.copyArray(original)
    verify(copy !== original)
    compare(service.copyArray("not an array").length, 0)
    var normalized = service.normalizeMessages(original)
    compare(normalized[0].reactions.length, 1)
    compare(normalized[0].reactions[0], "👍")
    compare(normalized[0].receipt, 4)
    compare(normalized[0].delivered_at, 123)
    compare(normalized[0].read_at, 0)
    compare(normalized[0].delivered_to.length, 1)
    compare(normalized[0].delivered_to[0].name, "Bob")
    compare(normalized[0].delivered_to[0].delivered_at, 234)
    compare(normalized[0].read_by.length, 1)
    compare(normalized[0].read_by[0].name, "Alice")
    compare(normalized[0].read_by[0].read_at, 456)
    compare(normalized[1].reactions.length, 0)
    compare(normalized[1].receipt, 0)
    compare(normalized[1].delivered_at, 0)
    compare(normalized[1].read_at, 0)
    compare(normalized[1].delivered_to.length, 0)
    compare(normalized[1].read_by.length, 0)
    compare(service.hexKey("Aé"), "41c3a9")
  }

  function test_receipt_updates_are_monotonic_and_outgoing_only() {
    service.messages = [
      {
        id: "outgoing", from_me: true, receipt: 1,
        delivered_at: 0, read_at: 0, delivered_to: [], read_by: []
      },
      {
        id: "direct-read", from_me: true, receipt: 1,
        delivered_at: 0, read_at: 0, delivered_to: [], read_by: []
      },
      {
        id: "incoming", from_me: false, receipt: 0,
        delivered_to: [], read_by: []
      }
    ]
    compare(service.applyReceipts(null), false)
    compare(service.applyReceipts({
      message_ids: ["outgoing"], receipt: 2, timestamp: 20,
      delivery: {
        jid: "alice@s.whatsapp.net", name: "Alice", delivered_at: 19
      }
    }), true)
    compare(messagesWillChangeCount, 1)
    compare(lastPreservePosition, true)
    compare(service.messages[0].delivered_at, 20)
    compare(service.messages[0].delivered_to.length, 1)
    compare(service.messages[0].delivered_to[0].name, "Alice")
    compare(service.messages[0].delivered_to[0].delivered_at, 19)
    compare(service.applyReceipts({
      message_ids: ["outgoing"], receipt: 2, timestamp: 30,
      delivery: { jid: "alice@s.whatsapp.net", name: "Alice" }
    }), false)
    compare(messagesWillChangeCount, 1)
    compare(service.applyReceipts({
      message_ids: ["outgoing"], receipt: 2, timestamp: 10,
      delivery: {
        jid: "alice@s.whatsapp.net", name: "Alice Updated", delivered_at: 9
      }
    }), true)
    compare(service.messages[0].delivered_at, 10)
    compare(service.messages[0].delivered_to[0].name, "Alice Updated")
    compare(service.messages[0].delivered_to[0].delivered_at, 9)
    compare(service.applyReceipts({
      message_ids: ["outgoing"],
      receipt: 3,
      timestamp: 40,
      reader: {
        jid: "alice@s.whatsapp.net", name: "Alice", read_at: 39
      }
    }), true)
    compare(service.messages[0].receipt, 3)
    compare(service.messages[0].read_at, 40)
    compare(service.messages[0].read_by.length, 1)
    compare(service.messages[0].read_by[0].name, "Alice")
    compare(service.messages[0].read_by[0].read_at, 39)
    compare(service.applyReceipts({ message_ids: ["outgoing"], receipt: 2 }), false)
    compare(service.messages[0].receipt, 3)
    compare(service.applyReceipts({
      message_ids: ["outgoing"],
      receipt: 3,
      timestamp: 45,
      reader: { jid: "bob@s.whatsapp.net", name: "Bob" }
    }), true)
    compare(service.messages[0].read_by.length, 2)
    compare(service.messages[0].read_by[1].read_at, 45)
    compare(service.applyReceipts({
      message_ids: ["outgoing"],
      receipt: 3,
      timestamp: 50,
      reader: { jid: "bob@s.whatsapp.net", name: "Bob" }
    }), false)
    compare(service.applyReceipts({
      message_ids: ["outgoing"], receipt: 3, timestamp: 30,
      reader: { jid: "alice@s.whatsapp.net", name: "Alice", read_at: 29 }
    }), true)
    compare(service.messages[0].read_at, 30)
    compare(service.messages[0].read_by[0].read_at, 29)
    compare(service.applyReceipts({
      message_ids: ["direct-read"], receipt: 3, timestamp: 60,
      reader: { jid: "alice@s.whatsapp.net", name: "Alice", read_at: "invalid" }
    }), true)
    compare(service.messages[1].delivered_at, 0)
    compare(service.messages[1].read_at, 60)
    compare(service.messages[1].read_by[0].read_at, 0)
    compare(service.applyReceipts({ message_ids: ["incoming"], receipt: 4 }), false)
    compare(service.messages[2].receipt, 0)

    service.replaceMessages([{ id: "replacement" }], false)
    compare(messagesWillChangeCount, 7)
    compare(lastPreservePosition, false)
    compare(service.messages[0].id, "replacement")
  }

  function test_ui_preferences() {
    service.loadUiPreferences("{invalid")
    compare(service.uiPreferencesReady, true)
    compare(service.unreadOnly, false)
    service.loadUiPreferences('{"unread_only":true}')
    compare(service.unreadOnly, false)

    service.setUnreadOnly(true)
    compare(service.unreadOnly, true)
    compare(service.uiPreferencesRevision, 1)
    service.setUnreadOnly(true)
    compare(service.uiPreferencesRevision, 1)
    service.flushUiPreferences()
    compare(TestIo.files.length, 1)
    verify(TestIo.files[0].contents.indexOf('"unread_only": true') >= 0)
  }

  function test_ui_preferences_valid() {
    service.loadUiPreferences('{"unread_only":true}')
    compare(service.uiPreferencesReady, true)
    compare(service.unreadOnly, true)
  }

  function test_urls_and_external_actions() {
    compare(service.avatarUrl("me"), "")
    compare(service.avatarUrl("missing"), "")
    service.avatarAvailable = { "a@s.whatsapp.net": true }
    service.avatarRevisions = { "a@s.whatsapp.net": 3 }
    verify(service.avatarUrl("a@s.whatsapp.net").endsWith("6140732e77686174736170702e6e6574.img?v=3"))
    compare(service.fileUrl("", 2), "")
    compare(service.fileUrl("/tmp/file", 2), "file:///tmp/file?v=2")
    service.mediaRevision = 9
    compare(service.fileUrl("/tmp/file"), "file:///tmp/file?v=9")

    service.requestAvatar("me")
    compare(TestIo.socketWrites.length, 0)
    service.requestAvatar("contact")
    compare(lastFrame().command, "request_avatar")

    service.openMap("bad", 1)
    service.openMap(523700000, 48900000)
    verify(TestIo.processStarts.length > 0)
    verify(String(TestIo.processStarts[TestIo.processStarts.length - 1][1]).indexOf("52.37") >= 0)

    Quickshell.reset()
    service.openFile("")
    service.saveFile("", "")
    compare(Quickshell.detachedCommands.length, 0)
    service.openFile("/tmp/document")
    service.saveFile("/tmp/document", "name.pdf")
    compare(Quickshell.detachedCommands.length, 2)
    compare(Quickshell.detachedCommands[1][5], "name.pdf")
  }

  function test_message_request_deduplication_and_stale_responses() {
    compare(service.requestMessages(""), 0)
    var first = service.requestMessages("chat", false)
    verify(first > 0)
    compare(service.messagesQueuedRequests.chat, undefined)
    compare(service.requestMessages("chat", false), first)
    compare(service.messagesQueuedRequests.chat, undefined)
    compare(service.requestMessages("chat", true), first)
    compare(service.messagesQueuedRequests.chat, true)
    compare(service.finishMessagesRequest(null), "")
    compare(service.finishMessagesRequest({ id: 999 }), "")
    compare(service.finishMessagesRequest({ id: first }), "chat")
    compare(service.messagesRequestIds.chat, undefined)
    compare(service.finishMessagesRequest({ id: first }), "")
  }

  function test_group_participant_requests() {
    compare(service.requestGroupParticipants(""), false)
    compare(service.requestGroupParticipants("group@g.us"), true)
    compare(service.groupParticipantRequestPending("group@g.us"), true)
    compare(service.requestGroupParticipants("group@g.us"), false)
    var id = Number(Object.keys(service.groupParticipantRequestJids)[0])
    compare(service.finishGroupParticipantsRequest(null), "")
    compare(service.finishGroupParticipantsRequest({ id: 999 }), "")
    compare(service.finishGroupParticipantsRequest({ id: id }), "group@g.us")
    compare(service.groupParticipantRequestPending("group@g.us"), false)

    service.chats = [{ jid: "direct", is_group: false }, { jid: "group@g.us", is_group: true }]
    service.selectedChatJid = "direct"
    compare(service.refreshSelectedGroupParticipants(), false)
    service.selectedChatJid = "group@g.us"
    compare(service.refreshSelectedGroupParticipants(), true)
  }

  function test_media_download_lifecycle() {
    var message = { id: "m1", chat_jid: "chat", media: { kind: "image", path: "old" } }
    compare(service.mediaDownloadKey(null), "")
    compare(service.downloadMedia(null), false)
    compare(service.downloadMedia({ id: "m", chat_jid: "c", media: { kind: "document" } }), false)
    compare(service.downloadMedia(message), true)
    compare(lastFrame().command, "download_image")
    compare(service.mediaDownloading(message), true)
    compare(service.downloadMedia(message), false)
    var requestId = Number(Object.keys(service.mediaDownloadRequestIds)[0])
    service.finishMediaDownloadRequest(null)
    service.finishMediaDownloadRequest({ id: 999 })
    // The ack only confirms the daemon queued the transfer, so the message
    // stays busy until the broadcast reports the outcome.
    service.finishMediaDownloadRequest({ id: requestId })
    compare(Object.keys(service.mediaDownloadRequestIds).length, 0)
    compare(service.mediaDownloading(message), true)
    service.clearMediaDownloadState("chat", "missing")
    compare(service.mediaDownloading(message), true)
    service.clearMediaDownloadState("chat", "m1")
    compare(service.mediaDownloading(message), false)

    // A rejected request is never queued, so its marker clears immediately.
    compare(service.downloadMedia(message), true)
    requestId = Number(Object.keys(service.mediaDownloadRequestIds)[0])
    service.finishMediaDownloadRequest({ id: requestId, event: "error" })
    compare(service.mediaDownloading(message), false)

    var sticker = { id: "s1", chat_jid: "chat", media: {
      kind: "sticker", lottie: false
    } }
    compare(service.downloadMedia(sticker), true)
    compare(lastFrame().command, "download_sticker")
    compare(service.downloadMedia({ id: "l1", chat_jid: "chat", media: {
      kind: "sticker", lottie: true
    } }), false)
    requestId = Number(Object.keys(service.mediaDownloadRequestIds)[0])
    service.finishMediaDownloadRequest({ id: requestId })
    service.clearMediaDownloadState("chat", "s1")
    compare(service.mediaDownloading(sticker), false)

    var automatic = { id: "s2", chat_jid: "chat", media: {
      kind: "sticker", downloaded: false, lottie: false
    } }
    compare(service.autoDownloadStickers(null), 0)
    compare(service.autoDownloadStickers([
      { id: "done", chat_jid: "chat", media: {
        kind: "sticker", downloaded: true, lottie: false
      } },
      { id: "lottie", chat_jid: "chat", media: {
        kind: "sticker", downloaded: false, lottie: true
      } },
      { id: "image", chat_jid: "chat", media: {
        kind: "image", downloaded: false
      } },
      automatic
    ]), 1)
    compare(lastFrame().command, "download_sticker")
    compare(service.mediaDownloading(automatic), true)
    compare(service.autoDownloadStickers([automatic]), 0)
    requestId = Number(Object.keys(service.mediaDownloadRequestIds)[0])
    service.finishMediaDownloadRequest({ id: requestId })
    service.clearMediaDownloadState("chat", "s2")

    service.selectedChatJid = "chat"
    service.messages = [message]
    compare(service.messageMedia(message).path, "old")
    compare(service.messageMediaRevision(message), "0-0")
    service.applyDownloadedMedia({ chat_jid: "other", message_id: "m1", media: { path: "wrong" } })
    service.applyDownloadedMedia({ chat_jid: "chat", message_id: "missing", media: { path: "wrong" } })
    service.applyDownloadedMedia({ chat_jid: "chat", message_id: "m1", media: { path: "new" } })
    compare(service.messageMedia(message).path, "new")
    compare(service.messageMediaRevision(message), "0-1")
  }

  function test_chat_selection_and_commands() {
    service.chats = [
      { jid: "direct", name: "Direct", is_group: false },
      { jid: "group@g.us", name: "Group", is_group: true }
    ]
    service.panelVisible = true
    service.panelFocused = true
    TestIo.socketWrites = []
    service.selectChat("group@g.us")
    compare(service.selectedChatJid, "group@g.us")
    compare(service.selectedChat.name, "Group")
    var commands = sentFrames().map(function(frame) { return frame.command })
    verify(commands.indexOf("get_messages") >= 0)
    verify(commands.indexOf("get_group_participants") >= 0)
    verify(commands.indexOf("mark_read") >= 0)
    verify(commands.indexOf("request_avatar") >= 0)
    verify(commands.indexOf("set_active_chat") >= 0)

    compare(service.sendMessage("  "), false)
    compare(service.sendMessage("hello"), true)
    var sendFrame = lastFrame()
    compare(sendFrame.command, "send_message")
    compare(sendFrame.text, "hello")
    verify(/^qml-[0-9]+-[0-9]+$/.test(sendFrame.delivery_id))
    compare(service.reactToMessage(null, "👍"), false)
    compare(service.reactToMessage({ id: "m1", sender_jid: "sender", from_me: false }, "👍"), true)
    var reaction = lastFrame()
    compare(reaction.command, "react")
    compare(reaction.target_from_me, false)
  }

  function test_presence_and_remote_chat_state_lifecycle() {
    service.presenceClock = new Date(2024, 7, 28, 15, 30, 0).getTime() / 1000
    service.handleLine(JSON.stringify({
      event: "presence", jid: "alice@s.whatsapp.net", available: true
    }))
    compare(service.presenceLabel("alice@s.whatsapp.net",
      service.presenceClock, "HH:mm", Qt.locale("en_US")), "online")
    service.handleLine(JSON.stringify({
      event: "presence", jid: "alice@s.whatsapp.net", available: false,
      last_seen: new Date(2024, 7, 28, 13, 7, 0).getTime() / 1000
    }))
    compare(service.presenceLabel("alice@s.whatsapp.net",
      service.presenceClock, "HH:mm", Qt.locale("en_US")),
      "last seen today at 13:07")
    compare(service.applyPresence(null), false)

    service.handleLine(JSON.stringify({
      event: "chat_state", chat_jid: "alice@s.whatsapp.net",
      sender_jid: "alice@s.whatsapp.net", sender_name: "Alice", state: "typing"
    }))
    compare(service.chatStateLabel("alice@s.whatsapp.net", false), "typing…")
    service.handleLine(JSON.stringify({
      event: "chat_state", chat_jid: "team@g.us",
      sender_jid: "bob@s.whatsapp.net", sender_name: "Bob", state: "typing"
    }))
    service.handleLine(JSON.stringify({
      event: "chat_state", chat_jid: "team@g.us",
      sender_jid: "alice@s.whatsapp.net", sender_name: "Alice", state: "recording"
    }))
    compare(service.chatStateLabel("team@g.us", true),
      "Alice is recording audio…; Bob is typing…")
    compare(service.applyChatState({ chat_jid: "team@g.us", sender_jid: "bob", state: "bad" }), false)
    compare(service.applyChatState({
      chat_jid: "team@g.us", sender_jid: "bob@s.whatsapp.net", state: "paused"
    }), true)
    compare(service.chatStateLabel("team@g.us", true), "Alice is recording audio…")
    compare(service.expireChatStates(service.presenceClock + 11), true)
    compare(service.chatStateLabel("team@g.us", true), "")

    service.handleLine(JSON.stringify({
      event: "state", status: { state: "disconnected" }, unread_total: 0
    }))
    compare(Object.keys(service.contactPresence).length, 0)
    compare(Object.keys(service.chatStates).length, 0)
  }

  function test_local_typing_and_presence_commands() {
    service.connectionState = "connected"
    service.selectedChatJid = "alice@s.whatsapp.net"
    service.setPanelState(true, true)
    var frames = sentFrames()
    verify(frames.some(function(frame) {
      return frame.command === "set_presence" && frame.available === true
    }))
    verify(frames.some(function(frame) {
      return frame.command === "set_active_chat"
        && frame.chat_jid === "alice@s.whatsapp.net"
    }))

    TestIo.socketWrites = []
    compare(service.noteComposerActivity("h"), true)
    compare(service.noteComposerActivity("hi"), true)
    frames = sentFrames().filter(function(frame) {
      return frame.command === "set_chat_state"
    })
    compare(frames.length, 1)
    compare(frames[0].state, "typing")
    compare(service.pauseComposing(), true)
    compare(lastFrame().state, "paused")
    compare(service.pauseComposing(), false)

    service.noteComposerActivity("again")
    service.setPanelState(true, false)
    frames = sentFrames()
    verify(frames.some(function(frame) {
      return frame.command === "set_presence" && frame.available === false
    }))
    verify(frames.some(function(frame) {
      return frame.command === "set_chat_state" && frame.state === "paused"
    }))
    compare(service.noteComposerActivity("not focused"), false)
  }

  function test_voice_recording_and_send_lifecycle() {
    service.connectionState = "connected"
    service.selectedChatJid = "alice@s.whatsapp.net"
    service.setPanelState(true, true)
    TestIo.socketWrites = []
    var recording = service.newVoiceRecording()
    verify(service.recordingIdIsValid(recording.recording_id))
    verify(recording.path.indexOf(service.statePath + "/outbox/voice-") === 0)
    verify(recording.path.endsWith(recording.recording_id + ".ogg"))
    compare(recording.chat_jid, "alice@s.whatsapp.net")
    compare(service.beginVoiceRecording(), true)
    compare(service.localChatState, "recording")
    compare(lastFrame().command, "set_chat_state")
    compare(lastFrame().state, "recording")
    compare(service.finishVoiceRecording(), true)
    compare(lastFrame().state, "paused")

    compare(service.sendVoiceMessage("../private", recording.chat_jid, 1000), false)
    compare(service.sendVoiceMessage(recording.recording_id, recording.chat_jid, 100), false)
    compare(service.sendVoiceMessage(recording.recording_id, recording.chat_jid, 2400), true)
    var sendVoice = lastFrame()
    compare(sendVoice.command, "send_voice_message")
    compare(sendVoice.chat_jid, "alice@s.whatsapp.net")
    compare(sendVoice.recording_id, recording.recording_id)
    compare(sendVoice.path, undefined)
    compare(sendVoice.duration_ms, undefined)
    compare(service.voiceOutboxEntries.length, 1)
    compare(service.voiceOutboxEntries[0].status, "sending")
    compare(service.sendVoiceMessage(recording.recording_id,
      recording.chat_jid, 2400), false)
    compare(service.finishVoiceMessageRequest({ id: sendVoice.id + 1 }), false)
    service.handleLine(JSON.stringify({
      id: sendVoice.id,
      event: "error",
      message: "offline"
    }))
    compare(service.voiceMessageRequestId, 0)
    compare(service.voiceOutboxEntries[0].status, "failed")
    compare(service.voiceOutboxEntries[0].error, "offline")

    compare(service.retryVoiceMessage(service.voiceOutboxEntries[0]), true)
    compare(service.voiceMessageRequestRecordingId, recording.recording_id)
    service.handleState({ status: { state: "connecting" } })
    compare(service.voiceMessageRequestId, 0)
    compare(service.voiceMessageRequestRecordingId, "")
    compare(service.voiceOutboxEntries[0].status, "failed")
    verify(service.voiceOutboxEntries[0].error.indexOf("retry is safe") >= 0)

    service.connectionState = "connected"
    compare(service.retryVoiceMessage(service.voiceOutboxEntries[0]), true)
    var retryVoice = lastFrame()
    compare(service.finishVoiceMessageRequest({
      id: retryVoice.id,
      event: "sent"
    }), true)
    compare(service.voiceOutboxEntries.length, 0)

    service.handleLine(JSON.stringify({ event: "voice_outbox", entries: [
      { recording_id: "bad/id", status: "failed", duration_ms: 2400 },
      {
        recording_id: recording.recording_id,
        chat_jid: recording.chat_jid,
        duration_ms: 2451,
        status: "failed",
        error: "retry",
        created_at: 42
      }
    ] }))
    compare(service.voiceOutboxEntries.length, 1)
    compare(service.voiceOutboxEntries[0].duration_ms, 2451)
    compare(service.voiceOutboxForChat(recording.chat_jid).recording_id,
      recording.recording_id)
    service.setLocalVoiceOutboxEntry({
      recording_id: "local-2",
      chat_jid: recording.chat_jid,
      duration_ms: 1000,
      status: "failed",
      local_only: true
    })
    compare(service.applyVoiceOutbox({ entries: [] }), 1)
    compare(service.voiceOutboxEntries[0].recording_id, "local-2")
    service.removeLocalVoiceOutboxEntry("local-2")
    service.applyVoiceOutbox({ entries: [{
      recording_id: recording.recording_id,
      chat_jid: recording.chat_jid,
      duration_ms: 2451,
      status: "failed"
    }]})
    compare(service.discardVoiceRecording("bad/id"), false)
    compare(service.discardVoiceRecording(recording.recording_id), true)
    compare(lastFrame().command, "discard_voice_recording")
    compare(lastFrame().recording_id, recording.recording_id)
    compare(service.voiceOutboxEntries.length, 0)
  }

  function test_set_chat_pinned_command() {
    compare(service.setChatPinned("", true), false)
    compare(TestIo.socketWrites.length, 0)
    compare(service.setChatPinned("chat@g.us", true), true)
    var pin = lastFrame()
    compare(pin.command, "set_chat_pinned")
    compare(pin.chat_jid, "chat@g.us")
    compare(pin.pinned, true)
    compare(service.setChatPinned("chat@g.us", false), true)
    compare(lastFrame().pinned, false)
  }

  function test_poll_creation_and_vote_lifecycle() {
    compare(service.createPoll("Question?", ["A", "B"], false), false)
    service.selectedChatJid = "chat"
    compare(service.createPoll("", ["A", "B"], false), false)
    compare(service.createPoll("Question?", ["A"], false), false)
    compare(service.createPoll("Question?", [" A ", "A", "B"], true), true)
    var create = lastFrame()
    compare(create.command, "create_poll")
    compare(create.question, "Question?")
    compare(create.options.length, 2)
    compare(create.options[0], "A")
    compare(create.selectable_count, 2)
    compare(service.createPoll("Again?", ["A", "B"], false), false)
    service.finishPollRequest({ id: create.id })
    compare(service.pollCreateRequestId, 0)

    var message = { id: "poll-1", chat_jid: "chat" }
    compare(service.pollVoteKey(null), "")
    compare(service.votePoll(null, ["A"]), false)
    compare(service.pollVotePending(message), false)
    compare(service.votePoll(message, ["A"]), true)
    var vote = lastFrame()
    compare(vote.command, "vote_poll")
    compare(vote.message_id, "poll-1")
    compare(vote.selected_options[0], "A")
    compare(service.pollVotePending(message), true)
    compare(service.votePoll(message, []), false)
    service.finishPollRequest({ id: vote.id })
    compare(service.pollVotePending(message), false)
    service.finishPollRequest(null)
  }

  function test_panel_and_shell_commands() {
    compare(service.openPanel("chat"), "unavailable")
    var summonArgs = []
    service.shell = {
      summon: function(pluginId, payload) {
        summonArgs = [pluginId, JSON.parse(payload)]
        return true
      }
    }
    compare(service.openPanel("chat"), "opened")
    compare(summonArgs[0], "test.whatsapp")
    compare(summonArgs[1].chatJid, "chat")
    service.selectedChatJid = "chat"
    service.setPanelState(true, true)
    var frames = sentFrames()
    verify(frames.some(function(frame) {
      return frame.command === "set_active_chat" && frame.chat_jid === "chat"
    }))
    service.setPanelState(false, true)
    frames = sentFrames()
    verify(frames.some(function(frame) {
      return frame.command === "set_active_chat" && frame.chat_jid === null
    }))
    compare(service.unlinkDevice(), false)
    service.connectionState = "connected"
    compare(service.unlinkDevice(), true)
  }

  function test_event_state_and_malformed_frames() {
    service.handleLine("")
    service.handleLine("not json")
    service.handleLine("null")
    service.handleLine("{}")
    service.handleLine('{"event":"state","status":{"state":"pairing","reason":"scan","expires_at":12},"unread_total":4}')
    compare(service.connectionState, "pairing")
    compare(service.connectionDetail, "scan")
    compare(service.pairingExpiresAt, 12)
    compare(service.unreadTotal, 4)
    service.handleLine('{"event":"unread","total":8}')
    compare(service.unreadTotal, 8)
    service.handleLine('{"event":"error"}')
    compare(service.lastError, "WhatsApp command failed")
  }

  function test_event_chats_messages_and_stale_selection() {
    service.panelVisible = true
    service.handleLine(JSON.stringify({ event: "chats", chats: [
      { jid: "one", is_group: false }, { jid: "two", is_group: false }
    ] }))
    compare(service.selectedChatJid, "one")

    var requestId = Number(service.messagesRequestIds.one)
    service.handleLine(JSON.stringify({ id: requestId, event: "messages", chat_jid: "two", messages: [{ id: "stale" }] }))
    compare(service.messages.length, 0)
    service.handleLine(JSON.stringify({ event: "messages", chat_jid: "one", first_unread_message_id: "m1", messages: [
      { id: "m1", sender_jid: "sender", reactions: null },
      { id: "m2", sender_jid: "sender", reactions: ["👍"] },
      { id: "m3", sender_jid: "me" },
      { id: "m4", chat_jid: "one", sender_jid: "sender", media: {
        kind: "sticker", downloaded: false, lottie: false
      } }
    ] }))
    compare(service.messages.length, 4)
    compare(service.messagesFirstUnreadId, "m1")
    compare(service.messagesResponseSerial, 1)
    compare(service.mediaRevision, 1)
    compare(service.mediaDownloading(service.messages[3]), true)
    verify(sentFrames().some(function(frame) {
      return frame.command === "download_sticker" && frame.message_id === "m4"
    }))

    requestId = service.requestMessages("one")
    service.handleLine(JSON.stringify({
      id: requestId,
      event: "messages",
      chat_jid: "one",
      messages: [{ id: "m1", sender_jid: "sender" }]
    }))
    compare(lastPreservePosition, true)
    compare(service.messagesResponseSerial, 2)
  }

  function test_event_groups_media_messages_and_errors() {
    service.chats = [{ jid: "group@g.us", is_group: true }]
    service.selectedChatJid = "group@g.us"
    var requestId = service.requestGroupParticipants("group@g.us")
    requestId = Number(Object.keys(service.groupParticipantRequestJids)[0])
    service.handleLine(JSON.stringify({ id: requestId, event: "group_participants", chat_jid: "other", participants: [1] }))
    compare(service.groupParticipants.length, 0)
    requestId = service.requestGroupParticipants("group@g.us")
    requestId = Number(Object.keys(service.groupParticipantRequestJids)[0])
    service.handleLine(JSON.stringify({ id: requestId, event: "error", message: "failed" }))
    compare(service.groupParticipantsError, "failed")

    service.messages = [{ id: "m1", chat_jid: "group@g.us", media: { kind: "image" } }]
    service.mediaDownloadRequests = ({ "group@g.us\nm1": true })
    service.handleLine(JSON.stringify({ event: "media_downloaded", chat_jid: "group@g.us", message_id: "m1", media: { kind: "image", path: "/tmp/new" } }))
    compare(service.messageMedia(service.messages[0]).path, "/tmp/new")
    compare(service.mediaDownloading(service.messages[0]), false)

    // A background download that fails releases the message and reports why.
    service.mediaDownloadRequests = ({ "group@g.us\nm1": true })
    service.handleLine(JSON.stringify({
      event: "media_download_failed", chat_jid: "group@g.us", message_id: "m1",
      message: "WhatsApp did not return download details for this image"
    }))
    compare(service.mediaDownloading(service.messages[0]), false)
    compare(service.lastError, "WhatsApp did not return download details for this image")
    compare(service.lastErrorRequestId, "")
    service.mediaDownloadRequests = ({ "group@g.us\nm1": true })
    service.handleLine(JSON.stringify({
      event: "media_download_failed", chat_jid: "group@g.us", message_id: "m1"
    }))
    compare(service.lastError, "WhatsApp media download failed")

    service.messages = [
      { id: "own", chat_jid: "group@g.us", from_me: true, receipt: 0 },
      { id: "incoming", chat_jid: "group@g.us", from_me: false, receipt: 0 }
    ]
    service.handleLine(JSON.stringify({
      event: "receipts", chat_jid: "group@g.us", message_ids: ["own", "incoming"], receipt: 3
    }))
    compare(service.messages[0].receipt, 3)
    compare(service.messages[1].receipt, 0)
  }

  function test_event_sent_incoming_avatars_and_hello() {
    service.selectedChatJid = "chat"
    service.messages = [{ id: "sent", from_me: true, receipt: 1 }]
    service.handleLine(JSON.stringify({
      event: "receipts", message_ids: ["sent"], receipt: 3
    }))
    compare(service.messages[0].receipt, 3)
    service.handleLine(JSON.stringify({ event: "sent", message: { chat_jid: "chat" } }))
    compare(service.messageSentSerial, 1)
    service.handleLine(JSON.stringify({ event: "message", message: { chat_jid: "chat" } }))
    compare(service.incomingMessageSerial, 1)
    service.handleLine(JSON.stringify({ event: "message", message: { chat_jid: "other" } }))
    compare(service.incomingMessageSerial, 1)

    service.handleLine(JSON.stringify({ event: "avatars", jids: ["a", "b"], changed_jids: ["b"], revision: 7 }))
    compare(service.avatarAvailable.a, true)
    compare(service.avatarRevisions.b, 7)
    service.handleLine(JSON.stringify({
      event: "hello", protocol_version: service.protocolVersion
    }))
    verify(sentFrames().length > 0)
  }

  function test_text_message_is_only_complete_after_durable_acceptance() {
    service.selectedChatJid = "chat"
    compare(service.sendMessage("keep this draft"), true)
    var request = lastFrame()
    compare(request.command, "send_message")
    verify(String(request.delivery_id).length > 0)
    compare(textAcceptedCount, 0)
    verify(service.textMessageRequests[String(request.id)] !== undefined)

    service.handleLine(JSON.stringify({ event: "pong" }))
    compare(textAcceptedCount, 0)
    service.handleLine(JSON.stringify({
      id: request.id,
      event: "text_accepted",
      delivery_id: request.delivery_id
    }))
    compare(textAcceptedCount, 1)
    compare(acceptedDeliveryId, request.delivery_id)
    compare(acceptedText, "keep this draft")
    compare(service.textMessageRequests[String(request.id)], undefined)

    service.handleLine(JSON.stringify({
      event: "text_outbox",
      entries: [{
        delivery_id: request.delivery_id,
        chat_jid: "chat",
        text: "keep this draft",
        status: "failed",
        error: "offline"
      }]
    }))
    compare(service.textOutboxEntries.length, 1)
    var failed = service.textOutboxForChat("chat")
    compare(failed.delivery_id, request.delivery_id)
    compare(service.retryTextMessage(null), false)
    compare(service.retryTextMessage(failed), true)
    compare(lastFrame().command, "retry_text_message")
    compare(service.textOutboxEntries[0].status, "queued")
    compare(service.textOutboxForChat("chat"), null)
    service.textOutboxEntries = [failed]
    compare(service.discardTextMessage({ delivery_id: "", status: "failed" }), false)
    compare(service.discardTextMessage({ delivery_id: "sending", status: "sending" }), false)
    compare(service.discardTextMessage(failed), true)
    compare(lastFrame().command, "discard_text_message")
    compare(service.textOutboxEntries.length, 0)
    service.handleLine(JSON.stringify({
      event: "text_delivery",
      delivery_id: request.delivery_id,
      status: "failed",
      error: "synthetic delivery failure"
    }))
    compare(service.lastError, "synthetic delivery failure")
    compare(lastFrame().command, "list_text_outbox")
  }

  function test_generation_and_resource_sequences_prevent_regression() {
    service.handleLine(JSON.stringify({
      event: "chats", generation: 4, sequence: 10,
      chats: [{ jid: "new", is_group: false }]
    }))
    compare(service.daemonGeneration, 4)
    compare(service.chats[0].jid, "new")
    service.handleLine(JSON.stringify({
      event: "chats", generation: 4, sequence: 9,
      chats: [{ jid: "stale", is_group: false }]
    }))
    compare(service.chats[0].jid, "new")
    service.handleLine(JSON.stringify({
      event: "chats", generation: 3, sequence: 99,
      chats: [{ jid: "old-generation", is_group: false }]
    }))
    compare(service.chats[0].jid, "new")
    service.handleLine(JSON.stringify({
      event: "chats", generation: 5, sequence: 1,
      chats: [{ jid: "next-generation", is_group: false }]
    }))
    compare(service.daemonGeneration, 5)
    compare(service.chats[0].jid, "next-generation")
  }

  function test_invalidations_refresh_only_the_affected_resource() {
    service.selectedChatJid = "chat"
    TestIo.socketWrites = []
    service.handleLine(JSON.stringify({
      event: "invalidated", resource: "chats", generation: 1, sequence: 1
    }))
    compare(lastFrame().command, "list_chats")
    service.handleLine(JSON.stringify({
      event: "invalidated", resource: "messages", key: "other",
      generation: 1, sequence: 2
    }))
    compare(lastFrame().command, "list_chats")
    service.handleLine(JSON.stringify({
      event: "invalidated", resource: "messages", key: "chat",
      generation: 1, sequence: 3
    }))
    compare(lastFrame().command, "get_messages")
    service.handleLine(JSON.stringify({
      event: "invalidated", resource: "unread", generation: 1, sequence: 4
    }))
    compare(lastFrame().command, "get_state")
    service.handleLine(JSON.stringify({
      event: "invalidated", resource: "avatars", generation: 1, sequence: 5
    }))
    compare(lastFrame().command, "list_avatars")
    service.handleLine(JSON.stringify({
      event: "invalidated", resource: "text_outbox", generation: 1, sequence: 6
    }))
    compare(lastFrame().command, "list_text_outbox")
    service.handleLine(JSON.stringify({
      event: "invalidated", resource: "voice_outbox", generation: 1, sequence: 7
    }))
    compare(lastFrame().command, "list_voice_outbox")
    compare(service.handleInvalidation({ resource: "unknown" }), false)
  }

  function test_read_intent_requires_visible_focus() {
    service.chats = [{ jid: "one", is_group: false }, { jid: "two", is_group: false }]
    TestIo.socketWrites = []
    service.selectChat("one")
    verify(!sentFrames().some(function(frame) { return frame.command === "mark_read" }))
    compare(service.markSelectedRead(), false)
    service.panelVisible = true
    verify(!sentFrames().some(function(frame) { return frame.command === "mark_read" }))
    service.selectChat("two")
    verify(!sentFrames().some(function(frame) { return frame.command === "mark_read" }))
    service.panelFocused = true
    verify(sentFrames().some(function(frame) {
      return frame.command === "mark_read" && frame.chat_jid === "two"
    }))
    TestIo.socketWrites = []
    service.selectChat("one")
    verify(sentFrames().some(function(frame) {
      return frame.command === "mark_read" && frame.chat_jid === "one"
    }))
  }

  function test_request_timeout_and_reconnect_clear_all_transient_maps() {
    service.selectedChatJid = "chat"
    compare(service.sendMessage("retry me"), true)
    var request = lastFrame()
    service.requestDeadlines[String(request.id)] = 1
    compare(service.expireRequests(2), 1)
    compare(service.textMessageRequests[String(request.id)], undefined)
    verify(service.lastError.indexOf("send message timed out") >= 0)

    service.mediaDownloadRequests = ({ "chat\nm1": true })
    service.mediaDownloadRequestIds = ({ "999": "chat\nm1" })
    service.messagesRequestIds = ({ chat: 998 })
    service.messagesRequestJids = ({ "998": "chat" })
    var socket = TestIo.sockets[TestIo.sockets.length - 1]
    socket.connected = false
    compare(Object.keys(service.mediaDownloadRequests).length, 0)
    compare(Object.keys(service.mediaDownloadRequestIds).length, 0)
    compare(Object.keys(service.messagesRequestIds).length, 0)
    compare(Object.keys(service.messagesRequestJids).length, 0)
  }

  function test_unrelated_frames_do_not_clear_action_error() {
    service.handleLine(JSON.stringify({ id: 4242, event: "error", message: "keep me" }))
    compare(service.lastError, "keep me")
    service.handleLine(JSON.stringify({ event: "unread", total: 3 }))
    compare(service.lastError, "keep me")
    service.selectedChatJid = "chat"
    compare(service.sendMessage("new action"), true)
    compare(service.lastError, "")
  }

  function test_reconnect_sequence() {
    verify(TestIo.sockets.length > 0)
    var socket = TestIo.sockets[TestIo.sockets.length - 1]
    socket.error("disconnected")
    compare(service.connected, false)
    compare(service.connectionState, "starting")
    tryVerify(function() { return TestIo.sockets.length > 1 })
    tryCompare(service, "connected", true)
    compare(service.reconnectAttempt, 0)
    compare(service.protocolCompatible, false)
    service.handleLine(JSON.stringify({
      event: "hello", protocol_version: service.protocolVersion
    }))
    compare(service.protocolCompatible, true)
  }
}
