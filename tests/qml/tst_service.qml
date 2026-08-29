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

  Connections {
    target: testCase.service
    function onMessagesWillChange(preservePosition) {
      testCase.messagesWillChangeCount++
      testCase.lastPreservePosition = preservePosition
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
    TestIo.socketWrites = []
    TestIo.processStarts = []
    messagesWillChangeCount = 0
    lastPreservePosition = false
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

    service.refreshMetadata()
    var frames = sentFrames()
    compare(frames[frames.length - 3].command, "get_state")
    compare(frames[frames.length - 2].command, "list_chats")
    compare(frames[frames.length - 1].command, "list_avatars")
  }

  function test_copy_normalize_and_hex_helpers() {
    var original = [
      {
        id: "one",
        receipt: 9,
        delivered_at: "123.9",
        read_at: "invalid",
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
    compare(normalized[0].read_by.length, 1)
    compare(normalized[0].read_by[0].name, "Alice")
    compare(normalized[0].read_by[0].read_at, 456)
    compare(normalized[1].reactions.length, 0)
    compare(normalized[1].receipt, 0)
    compare(normalized[1].delivered_at, 0)
    compare(normalized[1].read_at, 0)
    compare(normalized[1].read_by.length, 0)
    compare(service.hexKey("Aé"), "41c3a9")
  }

  function test_receipt_updates_are_monotonic_and_outgoing_only() {
    service.messages = [
      {
        id: "outgoing", from_me: true, receipt: 1,
        delivered_at: 0, read_at: 0, read_by: []
      },
      {
        id: "direct-read", from_me: true, receipt: 1,
        delivered_at: 0, read_at: 0, read_by: []
      },
      { id: "incoming", from_me: false, receipt: 0, read_by: [] }
    ]
    compare(service.applyReceipts(null), false)
    compare(service.applyReceipts({
      message_ids: ["outgoing"], receipt: 2, timestamp: 20
    }), true)
    compare(messagesWillChangeCount, 1)
    compare(lastPreservePosition, true)
    compare(service.messages[0].delivered_at, 20)
    compare(service.applyReceipts({
      message_ids: ["outgoing"], receipt: 2, timestamp: 30
    }), false)
    compare(messagesWillChangeCount, 1)
    compare(service.applyReceipts({
      message_ids: ["outgoing"], receipt: 2, timestamp: 10
    }), true)
    compare(service.messages[0].delivered_at, 10)
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
    service.finishMediaDownloadRequest({ id: requestId })
    compare(service.mediaDownloading(message), false)

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
    compare(lastFrame().command, "send_message")
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
      { id: "m3", sender_jid: "me" }
    ] }))
    compare(service.messages.length, 3)
    compare(service.messagesFirstUnreadId, "m1")
    compare(service.messagesResponseSerial, 1)
    compare(service.mediaRevision, 1)

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
    service.handleLine(JSON.stringify({ event: "media_downloaded", chat_jid: "group@g.us", message_id: "m1", media: { kind: "image", path: "/tmp/new" } }))
    compare(service.messageMedia(service.messages[0]).path, "/tmp/new")

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
    service.handleLine(JSON.stringify({ event: "hello" }))
    verify(sentFrames().length > 0)
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
  }
}
