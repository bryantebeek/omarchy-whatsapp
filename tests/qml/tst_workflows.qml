import QtQuick
import QtTest
import QtMultimedia
import Quickshell
import qs.Commons

import "../../quickshell" as Whatsapp
import "../../quickshell/Model.js" as Model
import "fixtures"

TestCase {
  id: testCase
  name: "UiWorkflows"

  property var panel: null
  property var service: null

  Component { id: panelComponent; Whatsapp.Panel {} }
  Component { id: serviceComponent; WorkflowService {} }

  function init() {
    failOnWarning(/.*/)
    Quickshell.reset()
    service = createTemporaryObject(serviceComponent, testCase)
    verify(service !== null)
    service.chats = [
      { jid: "alice@s.whatsapp.net", name: "Alice", last_message: "Lunch?", unread: 2, pinned: false, is_group: false },
      { jid: "team@g.us", name: "Team", last_message: "Release ready", unread: 0, pinned: true, is_group: true }
    ]
    panel = createTemporaryObject(panelComponent, testCase, { service: service })
    verify(panel !== null)
  }

  function cleanup() {
    wait(10)
    if (panel) panel.destroy()
    if (service) service.destroy()
    panel = null
    service = null
  }

  function control(name) {
    var item = findChild(panel, name)
    verify(item !== null, "Missing production control: " + name)
    return item
  }

  function callRecorded(name) {
    return callCount(name) > 0
  }

  function callCount(name) {
    var total = 0
    for (var i = 0; i < service.calls.length; i++)
      if (service.calls[i].name === name) total++
    return total
  }

  function verifyCenteredSquareButton(button, input) {
    compare(button.width, input.height)
    compare(button.height, input.height)
    compare(button.width, button.height)
    compare(button.centeredIconItem.x + button.centeredIconItem.width / 2,
      button.width / 2)
    compare(button.centeredIconItem.y + button.centeredIconItem.height / 2,
      button.height / 2)
    compare(button.centeredIconItem.horizontalAlignment, Text.AlignHCenter)
    compare(button.centeredIconItem.verticalAlignment, Text.AlignVCenter)
  }

  function syntheticMessages(count) {
    var messages = []
    for (var i = 0; i < count; i++) {
      messages.push({
        id: "scroll-" + i,
        chat_jid: "alice@s.whatsapp.net",
        sender_jid: i % 2 ? "me" : "alice@s.whatsapp.net",
        sender_name: i % 2 ? "" : "Alice",
        from_me: i % 2 === 1,
        text: "Synthetic message " + i + " with enough text for a stable row",
        timestamp: 1000 + i * 61,
        receipt: 1,
        read_by: []
      })
    }
    return messages
  }

  function fixturePath(name) {
    var path = String(Qt.resolvedUrl("fixtures/" + name))
    return decodeURIComponent(path.substring("file://".length))
  }

  function mappedBottom(item, target) {
    return item.mapToItem(target, 0, item.height).y
  }

  function test_open_search_select_and_close() {
    panel.open('{"chatJid":"team@g.us"}')
    compare(panel.opened, true)
    compare(service.selectedChatJid, "team@g.us")
    verify(callRecorded("refreshMetadata"))
    verify(callRecorded("setPanelState"))

    var search = control("chatSearch")
    var unreadFilterButton = control("unreadFilterButton")
    var newChatButton = control("newChatButton")
    verifyCenteredSquareButton(unreadFilterButton, search)
    verifyCenteredSquareButton(newChatButton, search)
    search.text = "alice"
    tryCompare(panel.filteredChats, "length", 1)
    compare(panel.filteredChats[0].jid, "alice@s.whatsapp.net")
    search.text = "release"
    tryCompare(panel.filteredChats, "length", 1)
    compare(panel.filteredChats[0].jid, "team@g.us")
    search.text = "nobody"
    tryCompare(panel.filteredChats, "length", 0)

    search.text = ""
    panel.chooseChat("alice@s.whatsapp.net")
    compare(service.selectedChatJid, "alice@s.whatsapp.net")
    panel.close()
    compare(panel.opened, false)
  }

  function test_second_open_renders_messages_retained_by_the_service() {
    panel.open('{"chatJid":"alice@s.whatsapp.net"}')
    service.loadMessages(syntheticMessages(3), "")
    tryCompare(control("messageList"), "count", 3)
    panel.close()
    panel.destroy()
    panel = null
    wait(10)

    // The shell's asynchronous Loader completes the panel before injecting
    // its long-lived service, which already owns the selected conversation.
    panel = createTemporaryObject(panelComponent, testCase)
    verify(panel !== null)
    compare(findChild(panel, "conversationMessageModel").count, 0)
    panel.service = service
    panel.open("{}")

    compare(service.messages.length, 3)
    tryCompare(control("messageList"), "count", 3)
    verify(control("messageDelegate-scroll-0") !== null)
  }

  function test_wrapped_message_text_starts_at_left_edge_data() {
    return [
      { tag: "incoming", outgoing: false, caption: false },
      { tag: "outgoing", outgoing: true, caption: false },
      { tag: "incoming-caption", outgoing: false, caption: true },
      { tag: "outgoing-caption", outgoing: true, caption: true }
    ]
  }

  function test_wrapped_message_text_starts_at_left_edge(data) {
    panel.open('{"chatJid":"alice@s.whatsapp.net"}')
    var messages = syntheticMessages(1)
    messages[0].from_me = data.outgoing
    messages[0].text = "Take a look https://example.com/models/"
      + Array(30).join("sample-model-") + "?from=search"
    if (data.caption)
      messages[0].media = { kind: "image", caption: messages[0].text }
    service.loadMessages(messages, "")
    tryCompare(control("messageList"), "count", 1)
    var text = control((data.caption ? "mediaCaptionText-" : "messageText-") + "scroll-0")
    verify(text.lineCount > 1)
    compare(text.horizontalAlignment, Text.AlignLeft)
    compare(text.effectiveHorizontalAlignment, Text.AlignLeft)
  }

  function test_message_bubble_fits_rendered_lines_data() {
    return [
      { tag: "short", text: "A short message" },
      { tag: "explicit-lines", text: "A longer first line\nShort" },
      { tag: "wrapped-link", text: "Look https://example.com/"
        + Array(14).join("sample-model-") + "?from=search" }
    ]
  }

  function test_message_bubble_fits_rendered_lines(data) {
    panel.open('{"chatJid":"alice@s.whatsapp.net"}')
    var messages = syntheticMessages(1)
    messages[0].from_me = true
    messages[0].text = data.text
    service.loadMessages(messages, "")
    tryCompare(control("messageList"), "count", 1)
    var text = control("messageText-scroll-0")
    var bubble = control("messageBubble-scroll-0")
    tryVerify(function () {
      return Math.abs(bubble.width - text.paintedWidth
        - bubble.horizontalPadding) <= 1
    })
    verify(bubble.width <= bubble.maximumWidth)
    if (data.tag !== "short")
      verify(text.lineCount > 1)
    var delegate = control("messageDelegate-scroll-0")
    compare(bubble.x + bubble.width, delegate.width - Style.space(18))
  }

  function test_mention_contact_name_and_open_dm() {
    panel.open('{"chatJid":"team@g.us"}')
    service.groupParticipants = [{
      jid: "316222@s.whatsapp.net", name: "Bob & Sons",
      aliases: ["246204789186724@lid"], is_me: false
    }]
    service.groupParticipantsChatJid = "team@g.us"
    service.loadMessages([{
      id: "mention-1",
      chat_jid: "team@g.us",
      sender_jid: "alice@s.whatsapp.net",
      sender_name: "Alice's profile",
      text: "Please ask @246204789186724 or @999",
      timestamp: 100
    }], "")
    tryCompare(control("messageList"), "count", 1)
    var delegate = control("messageDelegate-mention-1")
    compare(delegate.senderLabelText, "Alice's profile")
    var senderAvatar = control("senderAvatar-mention-1")
    compare(senderAvatar.jid, "alice@s.whatsapp.net")
    compare(senderAvatar.initials, "AP")
    compare(senderAvatar.size, Style.space(30))
    verify(delegate.renderedMessageText.indexOf("@Bob &amp; Sons") >= 0)
    verify(delegate.renderedMessageText.indexOf("mention:316222%40s.whatsapp.net") >= 0)
    verify(delegate.renderedMessageText.indexOf("@999") >= 0)

    control("messageText-mention-1")
      .linkActivated("mention:316222%40s.whatsapp.net")
    compare(service.selectedChatJid, "316222@s.whatsapp.net")
    verify(service.selectedChat !== null)
    compare(service.selectedChat.name, "Bob & Sons")
    compare(service.selectedChat.is_group, false)
    verify(callRecorded("selectChat"))

    service.receiveChats([
      { jid: "team@g.us", name: "Release Team", is_group: true },
      { jid: "alice@s.whatsapp.net", name: "Alice", is_group: false }
    ])
    compare(service.selectedChatJid, "316222@s.whatsapp.net")
    verify(service.selectedChat !== null)
    compare(service.selectedChat.name, "Bob & Sons")
    compare(control("conversationTitle").text, "Bob & Sons")
    compare(control("conversationAvatar").jid, "316222@s.whatsapp.net")
    compare(control("conversationAvatar").initials, "BS")
    compare(control("composer").enabled, true)
  }

  function test_mentions_incoming_and_outgoing_data() {
    return [
      { tag: "others mention me", fromMe: false, token: "100444", expected: "You", self: true },
      { tag: "I mention others", fromMe: true, token: "100222", expected: "Bob", self: false },
      { tag: "others mention others", fromMe: false, token: "316222", expected: "Bob", self: false }
    ]
  }

  function test_mentions_incoming_and_outgoing(data) {
    panel.open('{"chatJid":"team@g.us"}')
    service.groupParticipantsChatJid = "team@g.us"
    service.loadMessages([{
      id: "mention-direction", chat_jid: "team@g.us",
      sender_jid: data.fromMe ? "me" : "alice@s.whatsapp.net",
      from_me: data.fromMe, text: "Hi @" + data.token, timestamp: 100
    }], "")
    tryCompare(control("messageList"), "count", 1)
    // Participant metadata can arrive after the messages.
    service.groupParticipants = [
      { jid: "316444@s.whatsapp.net", aliases: ["100444@lid"], name: "", is_me: true },
      { jid: "100222@lid", aliases: ["316222@s.whatsapp.net"], name: "Bob", is_me: false }
    ]
    var delegate = control("messageDelegate-mention-direction")
    tryVerify(function() {
      return delegate.renderedMessageText.indexOf("@" + data.expected) >= 0
    })
    if (data.self) {
      verify(delegate.renderedMessageText.indexOf("mention:") < 0)
    } else {
      control("messageText-mention-direction").linkActivated("mention:100222%40lid")
      compare(service.selectedChatJid, "100222@lid")
      compare(service.selectedChat.name, "Bob")
    }
  }

  function test_compose_group_mentions() {
    panel.open('{"chatJid":"team@g.us"}')
    service.groupParticipantsChatJid = "team@g.us"
    service.groupParticipants = [
      { jid: "100@lid", name: "Alice & Sons" },
      { jid: "200@s.whatsapp.net", name: "Bob" }
    ]
    var composer = control("composer")
    composer.text = "Hi @Al"
    composer.cursorPosition = composer.text.length
    tryCompare(control("mentionList"), "count", 1)
    control("mentionChoice-0").clicked()
    compare(composer.text, "Hi @Alice & Sons ")
    compare(panel.composerMentions.length, 1)
    composer.insert(composer.text.length, "and @Bo")
    composer.cursorPosition = composer.text.length
    tryCompare(control("mentionList"), "count", 1)
    composer.accepted()
    compare(composer.text, "Hi @Alice & Sons and @Bob ")
    composer.accepted()
    compare(service.sentMentionMessages[0].text, "Hi @100 and @200 ")
    compare(service.sentMentionMessages[0].mentions, ["100@lid", "200@s.whatsapp.net"])
    compare(composer.text, "")
    compare(panel.composerMentions, [])

    composer.text = "@Al"
    composer.cursorPosition = 3
    tryCompare(control("mentionList"), "count", 1)
    control("mentionChoice-0").clicked()
    composer.text = "removed mention"
    control("sendButton").clicked()
    compare(service.sentMentionMessages[1].mentions, [])

    composer.text = "@"
    composer.cursorPosition = 1
    tryCompare(control("mentionList"), "count", 2)
    panel.chooseChat("alice@s.whatsapp.net")
    compare(panel.composerMentionChoices, [])
    compare(panel.composerMentions, [])
  }

  function test_destroy_during_conversation_position_restore_is_safe() {
    panel.restoreConversationAfterMessages = true
    panel.scheduleConversationPositionRestore()
    panel.destroy()
    panel = null

    wait(10)
  }

  function test_daemon_setup_recovery() {
    service.connectionState = "starting"
    service.daemonSetupRequired = true
    service.daemonSetupDetail = "Preparing the local build…"
    panel.open("{}")
    compare(panel.opened, true)
    compare(panel.paired, false)
    compare(panel.daemonSetupRequired, true)
    compare(control("daemonSetupTitle").text, "Set up WhatsApp")
    compare(control("daemonSetupButton").text, "Build and start daemon")

    control("daemonSetupButton").click()
    verify(callRecorded("setupDaemonRuntime"))
    compare(service.daemonSetupBusy, true)
    compare(control("daemonSetupButton").enabled, false)
    compare(control("daemonSetupButton").text, "Setting up daemon…")

    service.daemonSetupBusy = false
    service.daemonSetupRequired = false
    control("daemonRetryButton").click()
    verify(callRecorded("retryDaemon"))
  }

  function test_new_conversation_and_send_message() {
    panel.open("{}")
    var newChat = control("newChat")
    panel.newChatVisible = true
    newChat.text = "+31 (6) 1234"
    control("openChatButton").click()
    compare(service.selectedChatJid, "3161234@s.whatsapp.net")
    verify(service.selectedChat !== null)
    compare(panel.newChatVisible, false)
    compare(newChat.text, "")

    var composer = control("composer")
    var composerButtons = [control("pollButton"),
      control("voiceRecordButton"), control("sendButton")]
    for (var index = 0; index < composerButtons.length; index++)
      verifyCenteredSquareButton(composerButtons[index], composer)
    var sendButton = control("sendButton")
    compare(sendButton.text, "")
    compare(sendButton.tooltipText, "Send message")
    compare(sendButton.enabled, true)
    sendButton.click()
    compare(service.sentMessages.length, 0)
    composer.text = "Hello from the test"
    compare(service.chatStateUpdates[service.chatStateUpdates.length - 1], "typing")
    sendButton.click()
    compare(service.sentMessages.length, 1)
    compare(service.sentMessages[0], "Hello from the test")
    compare(composer.text, "")
    compare(service.chatStateUpdates[service.chatStateUpdates.length - 1], "paused")
    composer.text = "same draft in another chat"
    service.selectedChatJid = "other@s.whatsapp.net"
    service.textMessageAccepted(
      "old-delivery", "3161234@s.whatsapp.net", "same draft in another chat")
    compare(composer.text, "same draft in another chat")
    service.textMessageAccepted(
      "current-delivery", "other@s.whatsapp.net", "same draft in another chat")
    compare(composer.text, "")

    service.textOutboxEntries = [{
      delivery_id: "failed-text",
      chat_jid: "other@s.whatsapp.net",
      text: "Could not send this",
      status: "failed",
      error: "Network unavailable"
    }]
    compare(control("textOutboxStatus").text, "Message failed  Could not send this")
    compare(control("textRetryButton").tooltipText, "Network unavailable")
    control("textRetryButton").click()
    compare(service.textOutboxEntries.length, 0)
    service.textOutboxEntries = [{
      delivery_id: "discard-text",
      chat_jid: "other@s.whatsapp.net",
      text: "Discard this",
      status: "failed"
    }]
    control("textOutboxDiscardButton").click()
    compare(service.textOutboxEntries.length, 0)
    wait(10)
  }

  function test_record_and_send_voice_message() {
    panel.open('{"chatJid":"alice@s.whatsapp.net"}')
    var recordButton = control("voiceRecordButton")
    compare(recordButton.enabled, true)
    compare(recordButton.tooltipText, "Record voice message")
    recordButton.click()
    compare(panel.voiceRecordingActive, true)
    compare(service.chatStateUpdates[service.chatStateUpdates.length - 1],
      "recording")
    var composer = control("composer")
    var recordingControls = control("voiceRecordingControls")
    var voiceCancelButton = control("voiceCancelButton")
    var voiceRecordingStatus = control("voiceRecordingStatus")
    var voiceSendButton = control("voiceSendButton")
    verifyCenteredSquareButton(voiceCancelButton, composer)
    verifyCenteredSquareButton(voiceSendButton, composer)
    compare(voiceCancelButton.width, recordButton.width)
    compare(voiceSendButton.width, recordButton.width)
    compare(voiceRecordingStatus.text, "●  0:02")
    tryVerify(function() {
      return voiceRecordingStatus.x > 0
        && voiceRecordingStatus.x + voiceRecordingStatus.width
          + recordingControls.spacing === voiceCancelButton.x
        && voiceCancelButton.x + voiceCancelButton.width
          + recordingControls.spacing === voiceSendButton.x
        && voiceSendButton.x + voiceSendButton.width === recordingControls.width
    })
    compare(voiceSendButton.enabled, true)
    voiceSendButton.click()
    compare(panel.voiceRecordingActive, false)
    compare(service.sentVoiceMessages.length, 1)
    compare(service.sentVoiceMessages[0].chat_jid, "alice@s.whatsapp.net")
    compare(service.sentVoiceMessages[0].recording_id, "42-1")
    compare(service.sentVoiceMessages[0].duration_ms, 2400)
    compare(service.discardedVoiceRecordings.length, 0)
    compare(service.chatStateUpdates[service.chatStateUpdates.length - 1],
      "paused")

    recordButton.click()
    compare(panel.voiceRecordingActive, true)
    voiceCancelButton.click()
    compare(panel.voiceRecordingActive, false)
    compare(service.sentVoiceMessages.length, 1)
    compare(service.discardedVoiceRecordings.length, 1)

    service.voiceOutboxEntries = [{
      recording_id: "retry-1",
      chat_jid: "alice@s.whatsapp.net",
      duration_ms: 2400,
      status: "failed",
      error: "Network unavailable"
    }]
    compare(control("voiceOutboxStatus").text, "Voice message failed  0:02")
    compare(panel.voiceRecordingActive, false)
    compare(panel.voiceOutboxEntry.status, "failed")
    var retryButton = control("voiceRetryButton")
    compare(service.connectionState, "connected")
    compare(service.voiceMessageRequestId, 0)
    tryCompare(retryButton, "enabled", true)
    compare(retryButton.tooltipText, "Network unavailable")
    retryButton.click()
    compare(service.sentVoiceMessages.length, 2)
    compare(service.sentVoiceMessages[1].recording_id, "retry-1")
    compare(service.voiceOutboxEntries.length, 0)

    service.voiceOutboxEntries = [{
      recording_id: "discard-2",
      chat_jid: "alice@s.whatsapp.net",
      duration_ms: 1000,
      status: "failed"
    }]
    control("voiceOutboxDiscardButton").click()
    compare(service.voiceOutboxEntries.length, 0)
    compare(service.discardedVoiceRecordings.length, 2)
  }

  function test_presence_and_typing() {
    panel.open('{"chatJid":"alice@s.whatsapp.net"}')
    var subtitle = control("conversationSubtitle")
    var alicePreview = null
    var teamPreview = null
    tryVerify(function() {
      alicePreview = findChild(panel, "chatPreview-alice@s.whatsapp.net")
      teamPreview = findChild(panel, "chatPreview-team@g.us")
      return alicePreview !== null && teamPreview !== null
    })
    compare(alicePreview.text, "Lunch?")
    compare(teamPreview.text, "Release ready")
    service.presenceLabelResult = "online"
    tryCompare(subtitle, "text", "online")
    service.chatStateLabels = ({ "alice@s.whatsapp.net": "typing…" })
    tryCompare(subtitle, "text", "typing…")
    compare(subtitle.showingChatActivity, true)
    compare(subtitle.color, panel.accent)
    tryCompare(alicePreview, "text", "Typing…")
    compare(alicePreview.color, panel.accent)
    compare(teamPreview.text, "Release ready")
    compare(String(teamPreview.color), String(panel.sidebarSecondary))
    service.chatStateLabels = ({})
    service.presenceLabelResult = "last seen today at 13:07"
    tryCompare(subtitle, "text", "last seen today at 13:07")
    compare(subtitle.showingChatActivity, false)
    compare(subtitle.color, panel.foreground)
    tryCompare(alicePreview, "text", "Lunch?")
    compare(String(alicePreview.color), String(panel.sidebarSecondary))

    panel.chooseChat("team@g.us")
    service.chatStateLabels = ({ "team@g.us": "Alice and Bob are typing…" })
    tryCompare(subtitle, "text", "Alice and Bob are typing…")
    compare(subtitle.color, panel.accent)
    tryCompare(teamPreview, "text", "Alice and Bob are typing…")
    compare(teamPreview.color, panel.accent)
  }

  function test_menus_open_without_auto_focus() {
    panel.open("{}")
    control("headerMoreButton").click()
    tryCompare(panel.appMenu, "opened", true)
    wait(10)
    compare(panel.appMenuFirstAction.activeFocus, false)
    panel.appMenu.close()

    var aliceRow = control("chatRow-alice@s.whatsapp.net")
    aliceRow.openContextMenuAt(aliceRow.width / 2, aliceRow.height / 2)
    tryCompare(aliceRow.contextMenu, "opened", true)
    wait(10)
    compare(aliceRow.pinAction.activeFocus, false)
    aliceRow.contextMenu.close()
  }

  function test_resync_chat_state_recovery() {
    panel.open("{}")
    control("headerMoreButton").click()
    tryCompare(panel.appMenu, "opened", true)
    var action = control("headerResyncAction")
    compare(action.visible, true)
    compare(action.enabled, true)
    compare(action.menuText, "Resync chat state")
    action.click()
    tryCompare(panel.chatStateResyncConfirmation, "opened", true)
    verify(panel.chatStateResyncConfirmation.message.indexOf(
      "messages, media, and local history stay intact") >= 0)
    panel.chatStateResyncConfirmation.confirmed()
    compare(panel.chatStateResyncConfirmation.opened, false)
    verify(callRecorded("requestChatStateResync"))
    compare(service.chatStateResyncStatus, "requested")

    control("headerMoreButton").click()
    tryCompare(panel.appMenu, "opened", true)
    compare(action.enabled, false)
    compare(action.menuText, "Resyncing chat state…")
    var status = control("headerResyncStatus")
    compare(status.visible, true)
    compare(status.text, "Chat-state resync requested")

    service.chatStateResyncStatus = "succeeded"
    service.chatStateResyncMessage = "WhatsApp chat state is up to date"
    compare(action.enabled, true)
    compare(action.menuText, "Resync chat state")
    compare(status.text, "WhatsApp chat state is up to date")

    service.chatStateResyncStatus = "failed"
    service.chatStateResyncMessage = "WhatsApp could not complete the replay"
    compare(action.enabled, true)
    compare(status.text, "WhatsApp could not complete the replay")
  }

  function test_pin_and_unpin_conversation() {
    panel.open("{}")
    var aliceRow = control("chatRow-alice@s.whatsapp.net")
    aliceRow.openContextMenuAt(aliceRow.width / 2, aliceRow.height / 2)
    var aliceMenu = aliceRow.contextMenu
    tryCompare(aliceMenu, "opened", true)
    var aliceAction = aliceRow.pinAction
    compare(aliceAction.menuText, "Pin conversation")
    aliceAction.click()
    compare(service.pinnedChats.length, 1)
    compare(service.pinnedChats[0].jid, "alice@s.whatsapp.net")
    compare(service.pinnedChats[0].pinned, true)

    var teamRow = control("chatRow-team@g.us")
    teamRow.openContextMenuAt(teamRow.width / 2, teamRow.height / 2)
    var teamMenu = teamRow.contextMenu
    tryCompare(teamMenu, "opened", true)
    var teamAction = teamRow.pinAction
    compare(teamAction.menuText, "Unpin conversation")
    teamAction.click()
    compare(service.pinnedChats.length, 2)
    compare(service.pinnedChats[1].jid, "team@g.us")
    compare(service.pinnedChats[1].pinned, false)
  }

  function test_load_history_and_receive_updates() {
    panel.open('{"chatJid":"alice@s.whatsapp.net"}')
    service.loadMessages([
      { id: "m1", chat_jid: "alice@s.whatsapp.net", sender_jid: "alice@s.whatsapp.net", sender_name: "Alice", text: "First", timestamp: 100 },
      {
        id: "m2",
        chat_jid: "alice@s.whatsapp.net",
        sender_jid: "me",
        from_me: true,
        text: "Second",
        timestamp: 101,
        receipt: 3,
        delivered_at: 102,
        read_at: 103,
        delivered_to: [{
          jid: "alice@s.whatsapp.net", name: "Alice", delivered_at: 102
        }, {
          jid: "bob@s.whatsapp.net", name: "Bob", delivered_at: 102
        }],
        read_by: [{
          jid: "alice@s.whatsapp.net", name: "Alice", read_at: 103
        }]
      }
    ], "m1")
    tryCompare(control("messageList"), "count", 2)
    var receiptStatus = control("messageReceiptStatus-m2")
    compare(receiptStatus.text, "✓✓")
    compare(receiptStatus.color, panel.accent)
    compare(receiptStatus.font.pixelSize, 14)
    compare(receiptStatus.font.letterSpacing, -3)
    compare(receiptStatus.parent.parent.spacing, 8)
    compare(receiptStatus.receiptTooltipText,
      "Read\nAlice · " + panel.messageReceiptTimestamp(103)
      + "\n\nDelivered\nBob · " + panel.messageReceiptTimestamp(102))
    var receiptHoverTarget = control("messageReceiptHoverTarget-m2")
    verify(receiptHoverTarget.width > 0,
      "Receipt hover target must have a positive width")
    verify(receiptHoverTarget.height > 0,
      "Receipt hover target must have a positive height")
    var receiptHoverArea = control("messageReceiptHoverArea-m2")
    compare(receiptHoverArea.hoverEnabled, true)
    compare(receiptHoverArea.acceptedButtons, Qt.NoButton)
    var receiptTooltip = receiptStatus.receiptTooltipControl
    verify(receiptTooltip !== null)
    compare(receiptTooltip.delay, 400)
    compare(receiptTooltip.timeout, -1)
    compare(receiptTooltip.padding, 0)
    compare(receiptTooltip.background.color, Color.tooltip.background)
    compare(receiptTooltip.background.radius, 0)
    compare(receiptTooltip.contentItem.groups.length, 2)
    compare(receiptTooltip.contentItem.groups[0].label, "Read")
    compare(receiptTooltip.contentItem.groups[0].entries[0],
      "Alice · " + panel.messageReceiptTimestamp(103))
    compare(receiptTooltip.contentItem.groups[1].label, "Delivered")
    compare(receiptTooltip.contentItem.groups[1].entries[0],
      "Bob · " + panel.messageReceiptTimestamp(102))
    compare(receiptTooltip.contentItem.groupSpacing, Style.space(6))
    compare(receiptTooltip.contentItem.headerColor, Qt.rgba(
      Color.tooltip.text.r, Color.tooltip.text.g, Color.tooltip.text.b,
      Color.tooltip.text.a * 0.72))
    compare(receiptTooltip.contentItem.detailColor, Color.tooltip.text)
    compare(receiptTooltip.contentItem.contentFontFamily, panel.fontFamily)
    compare(receiptTooltip.contentItem.contentFontSize, Style.font.bodySmall)
    compare(receiptTooltip.contentItem.leftInset,
      Border.left(receiptTooltip.tooltipBorderSpec)
      + Style.spacing.controlPaddingX)
    compare(receiptTooltip.contentItem.topInset,
      Border.top(receiptTooltip.tooltipBorderSpec)
      + Style.spacing.controlPaddingY)
    compare(control("messageTimestamp-m2").font.pixelSize, 10)
    compare(panel.messageIndex("m1"), 0)
    compare(panel.messageIndex("m2"), 1)
    compare(panel.messageIndex("missing"), -1)

    service.incomingMessageSerial++
    service.loadMessages(service.messages.concat([{
      id: "m3", chat_jid: "alice@s.whatsapp.net", sender_jid: "alice@s.whatsapp.net",
      sender_name: "Alice", text: "Third", timestamp: 102
    }]), "")
    tryCompare(control("messageList"), "count", 3)
  }

  function test_conversation_date_dividers() {
    panel.open('{"chatJid":"alice@s.whatsapp.net"}')
    var firstDay = new Date(2024, 1, 29, 9, 15, 0)
    var secondDay = new Date(2024, 2, 1, 10, 30, 0)
    service.loadMessages([
      {
        id: "date-1", chat_jid: "alice@s.whatsapp.net",
        sender_jid: "alice@s.whatsapp.net", sender_name: "Alice",
        text: "First day", timestamp: firstDay.getTime() / 1000
      },
      {
        id: "date-2", chat_jid: "alice@s.whatsapp.net",
        sender_jid: "alice@s.whatsapp.net", sender_name: "Alice",
        text: "Same day", timestamp: (firstDay.getTime() + 3600000) / 1000
      },
      {
        id: "date-3", chat_jid: "alice@s.whatsapp.net",
        sender_jid: "alice@s.whatsapp.net", sender_name: "Alice",
        text: "Next day", timestamp: secondDay.getTime() / 1000
      }
    ], "")

    var list = control("messageList")
    tryCompare(list, "count", 3)
    var firstDivider = control("dateDivider-date-1")
    var sameDayDivider = control("dateDivider-date-2")
    var nextDivider = control("dateDivider-date-3")
    compare(firstDivider.parent.showDateDivider, true)
    compare(sameDayDivider.parent.showDateDivider, false)
    compare(nextDivider.parent.showDateDivider, true)
    verify(firstDivider.height > 0)
    compare(sameDayDivider.height, 0)
    verify(nextDivider.height > 0)
    compare(firstDivider.bottomSpacing, Style.space(12))
    compare(firstDivider.height,
      firstDivider.contentHeight + firstDivider.bottomSpacing)
    compare(firstDivider.contentCenterOffset,
      -firstDivider.bottomSpacing / 2)
    verify(firstDivider.lineWidth > 0)
    verify(control("dateDividerLeftLine-date-1").width > 0)
    verify(control("dateDividerRightLine-date-1").width > 0)
    var firstDateLabel = control("dateDividerLabel-date-1")
    compare(firstDateLabel.text,
      firstDay.toLocaleDateString(Qt.locale(), "dddd, d MMMM yyyy"))
    compare(firstDateLabel.font.pixelSize, panel.messageMetaFontSize + 2)
    compare(control("dateDividerLabel-date-3").text,
      secondDay.toLocaleDateString(Qt.locale(), "dddd, d MMMM yyyy"))
    compare(control("messageBubble-date-1").y, firstDivider.height)
    compare(control("messageBubble-date-2").y, 0)
  }

  function test_event_updates_preserve_conversation_viewport() {
    panel.open('{"chatJid":"alice@s.whatsapp.net"}')
    service.loadMessages(syntheticMessages(40), "")
    var list = control("messageList")
    tryCompare(list, "count", 40)
    tryCompare(panel, "conversationReady", true)
    verify(list.model !== service.messages,
      "The ListView must use a stable render model instead of the service array")

    var aliceRow = control("chatRow-alice@s.whatsapp.net")
    var updatedChats = service.chats.slice()
    updatedChats[0] = Object.assign({}, updatedChats[0], {
      last_message: "Newest sidebar preview"
    })
    service.chats = updatedChats
    tryCompare(control("chatPreview-alice@s.whatsapp.net"), "text",
      "Newest sidebar preview")
    compare(control("chatRow-alice@s.whatsapp.net"), aliceRow,
      "Updating a chat preview must not recreate its sidebar row")

    list.positionViewAtIndex(18, ListView.Beginning)
    list.forceLayout()
    wait(20)
    var anchor = control("messageDelegate-scroll-18")
    var anchorOffset = anchor.y - list.contentY
    verify(list.contentY > 0)

    var updated = service.messages.slice()
    updated[4] = Object.assign({}, updated[4], {
      receipt: 3,
      text: "This event changed content above the viewport into a much taller "
        + "message. The visible anchor should remain at exactly the same pixel "
        + "offset even though the delegates above it now need more vertical "
        + "space after the conversation model is replaced and rendered again."
    })
    service.replaceMessages(updated, true)
    compare(panel.preservedConversationMessageOffset, anchorOffset)
    tryCompare(list, "count", 40)
    compare(JSON.parse(list.model.get(4).messageJson).receipt, 3)
    tryVerify(function() {
      var restored = findChild(panel, "messageDelegate-scroll-18")
      return restored !== null
        && Math.abs((restored.y - list.contentY) - anchorOffset) < 1
    })
    compare(control("messageDelegate-scroll-18"), anchor,
      "An unchanged visible bubble must not be recreated for metadata updates")

    service.incomingMessageSerial++
    service.loadMessages(service.messages.concat([{
      id: "scroll-40",
      chat_jid: "alice@s.whatsapp.net",
      sender_jid: "alice@s.whatsapp.net",
      sender_name: "Alice",
      text: "A new message received while reading history",
      timestamp: 4000
    }]), "")
    tryCompare(list, "count", 41)
    compare(list.model.get(40).messageKey, "id:scroll-40")
    tryVerify(function() {
      var restored = findChild(panel, "messageDelegate-scroll-18")
      return restored !== null
        && Math.abs((restored.y - list.contentY) - anchorOffset) < 1
    })
    compare(control("messageDelegate-scroll-18"), anchor,
      "Appending a message must not recreate visible history bubbles")

    panel.scheduleConversationScroll("bottom", "")
    panel.animateConversationViewportToBottom()
    tryVerify(function() { return panel.conversationViewportNearBottom() })
    var previousLatest = control("messageDelegate-scroll-40")
    service.incomingMessageSerial++
    service.loadMessages(service.messages.concat([{
      id: "scroll-41",
      chat_jid: "alice@s.whatsapp.net",
      sender_jid: "alice@s.whatsapp.net",
      sender_name: "Alice",
      text: "A new message received at the bottom",
      timestamp: 4061
    }]), "")
    tryCompare(list, "count", 42)
    tryVerify(function() { return panel.conversationViewportNearBottom() })
    compare(control("messageDelegate-scroll-40"), previousLatest,
      "Appending at the bottom must retain the previous latest bubble")
  }

  function test_conversation_has_no_load_earlier_messages_control() {
    panel.open('{"chatJid":"alice@s.whatsapp.net"}')
    service.loadMessages(syntheticMessages(40), "")
    var list = control("messageList")
    tryCompare(list, "count", 40)

    compare(findChild(panel, "loadOlderMessagesButton"), null)
    list.positionViewAtBeginning()
    list.movementEnded()
    compare(callCount("loadOlderMessages"), 0)
  }

  function test_create_render_and_vote_in_poll() {
    panel.open('{"chatJid":"alice@s.whatsapp.net"}')
    control("pollButton").click()
    tryCompare(control("createPollPopup"), "opened", true)
    control("pollQuestion").text = "Lunch?"
    control("pollOptions").text = "Soup\nSalad"
    control("createPollButton").click()
    compare(service.createdPolls.length, 1)
    compare(service.createdPolls[0].question, "Lunch?")
    compare(service.createdPolls[0].options.length, 2)

    service.loadMessages([{
      id: "poll-1",
      chat_jid: "alice@s.whatsapp.net",
      sender_jid: "alice@s.whatsapp.net",
      sender_name: "Alice",
      text: "[Poll] Lunch?",
      timestamp: 100,
      media: {
        kind: "poll",
        question: "Lunch?",
        selectable_count: 1,
        total_voters: 3,
        end_timestamp: 0,
        options: [
          { name: "Soup", votes: 2, selected_by_me: false,
            voter_jids: ["alice@s.whatsapp.net", "bob@s.whatsapp.net"] },
          { name: "Salad", votes: 1, selected_by_me: false,
            voter_jids: ["carol@s.whatsapp.net"] }
        ]
      }
    }], "")
    tryCompare(control("messageList"), "count", 1)
    var pollDelegate = control("messageDelegate-poll-1")
    compare(pollDelegate.mediaData.kind, "poll")
    compare(pollDelegate.isPoll, true)
    verify(control("pollCard-poll-1") !== null)
    var pollOptionButton = control("pollOptionButton-poll-1-0")
    var progressTrack = control("pollProgressTrack-poll-1-0")
    var progressFill = control("pollProgressFill-poll-1-0")
    var voterStack = control("pollVoterStack-poll-1-0")
    compare(control("pollOption-0").voterJids.length, 2)
    verify(voterStack.width > 0)
    tryVerify(function() {
      return findChild(panel, "pollVoterAvatar-poll-1-0-0") !== null
        && findChild(panel, "pollVoterAvatar-poll-1-0-1") !== null
    })
    var firstVoter = control("pollVoterAvatar-poll-1-0-0")
    var secondVoter = control("pollVoterAvatar-poll-1-0-1")
    compare(pollOptionButton.radius, control("messageBubble-poll-1").radius)
    verify(Border.color(pollOptionButton.borderSpec).a > 0)
    verify(Border.color(pollOptionButton.borderSpec).a <= 0.11)
    verify(progressTrack.height >= 8)
    compare(progressTrack.x, 0)
    compare(progressTrack.width, pollOptionButton.width)
    compare(progressTrack.y + progressTrack.height, pollOptionButton.height)
    verify(progressFill.width > progressTrack.width * 0.65)
    verify(progressFill.width < progressTrack.width * 0.68)
    compare(control("pollOptionCount-poll-1-0").text, "2")
    compare(firstVoter.jid, "alice@s.whatsapp.net")
    compare(secondVoter.jid, "bob@s.whatsapp.net")
    verify(secondVoter.x > firstVoter.x)
    verify(secondVoter.x < firstVoter.x + firstVoter.width)
    verify(voterStack.width > firstVoter.width)
    verify(service.calls.filter(function(call) {
      return call.name === "requestAvatar"
    }).length >= 3)
    compare(pollOptionButton.enabled, true)
    pollOptionButton.click()
    compare(service.pollVotes.length, 1)
    compare(service.pollVotes[0].selectedOptions[0], "Soup")

    service.pollVotePendingValue = true
    compare(pollOptionButton.enabled, false)
    service.pollVotePendingValue = false
    compare(pollOptionButton.enabled, true)

    var multiplePoll = Object.assign({}, service.messages[0])
    multiplePoll.id = "poll-multiple"
    service.avatarUrls = {
      "me": String(Qt.resolvedUrl("fixtures/pixel.svg"))
    }
    multiplePoll.media = Object.assign({}, multiplePoll.media, {
      selectable_count: 2,
      options: [
        { name: "Soup", votes: 1, selected_by_me: true,
          voter_jids: ["me"] },
        { name: "Salad", votes: 0, selected_by_me: false,
          voter_jids: [] },
        { name: "Bread", votes: 0, selected_by_me: false,
          voter_jids: [] }
      ]
    })
    service.loadMessages([multiplePoll], "")
    tryCompare(control("messageList"), "count", 1)
    verify(String(control("pollVoterImage-poll-multiple-0-0").source)
      .indexOf("/fixtures/pixel.svg") >= 0)
    control("pollOptionButton-poll-multiple-1").click()
    compare(service.pollVotes.length, 2)
    compare(service.pollVotes[1].selectedOptions.length, 2)
    compare(service.pollVotes[1].selectedOptions[0], "Soup")
    compare(service.pollVotes[1].selectedOptions[1], "Salad")

    multiplePoll.media = Object.assign({}, multiplePoll.media, {
      end_timestamp: 1
    })
    service.loadMessages([multiplePoll], "")
    compare(control("pollOptionButton-poll-multiple-0").enabled, false)
  }

  function test_open_media_and_recover_connection() {
    panel.open('{"chatJid":"alice@s.whatsapp.net"}')
    var imagePath = fixturePath("pixel.svg")
    service.loadMessages([{
      id: "text-style",
      chat_jid: "alice@s.whatsapp.net",
      sender_jid: "me",
      from_me: true,
      text: "Normal bubble",
      timestamp: 100
    }, {
      id: "image-style",
      chat_jid: "alice@s.whatsapp.net",
      sender_jid: "me",
      from_me: true,
      text: "[Image]",
      timestamp: 101,
      media: {
        kind: "image",
        path: imagePath,
        thumbnail_path: imagePath,
        downloaded: false,
        width: 1,
        height: 1
      }
    }], "")
    tryCompare(control("messageList"), "count", 2)
    var delegate = control("messageDelegate-image-style")
    var textBubble = control("messageBubble-text-style")
    var imageBubble = control("messageBubble-image-style")
    var mediaCard = control("mediaPreviewCard-image-style")
    var imageMask = control("mediaPreviewMask-image-style")
    var previewImage = control("mediaPreviewImage-image-style")
    var downloadButton = control("mediaDownloadButton-image-style")
    var timestamp = control("messageTimestamp-image-style")
    compare(imageBubble.showMessageBubble, false)
    compare(imageBubble.height, 0)
    compare(findChild(imageBubble, "mediaPreviewCard-image-style"), null)
    compare(mediaCard.topMargin, 0)
    compare(previewImage.y, 0)
    compare(previewImage.height, mediaCard.height)
    compare(imageMask.radius, textBubble.radius)
    compare(imageMask.y, 0)
    compare(previewImage.layer.enabled, true)
    compare(downloadButton.y + downloadButton.height / 2, mediaCard.height / 2)
    compare(mediaCard.x + mediaCard.width, delegate.width - Style.space(18))
    compare(textBubble.x + textBubble.width, delegate.width - Style.space(18))
    verify(mappedBottom(mediaCard, delegate)
      <= timestamp.parent.mapToItem(delegate, 0, 0).y)

    panel.openImagePreview(imagePath, "7", 1600, 900)
    verify(panel.imagePreviewUrl.endsWith("/fixtures/pixel.svg?v=7"))
    var viewer = control("imageViewerPopup")
    tryCompare(viewer, "visible", true)
    compare(viewer.width, viewer.parent.width)
    compare(viewer.height, viewer.parent.height)
    control("imageViewerCloseButton").click()
    tryCompare(panel, "imagePreviewUrl", "")

    service.connectionState = "disconnected"
    tryCompare(panel, "paired", false)
    service.lastError = "Network unavailable"
    compare(service.lastError, "Network unavailable")
    service.connectionState = "connected"
    service.lastError = ""
    tryCompare(panel, "paired", true)
  }

  function test_image_viewer_popup_fits_panel() {
    panel.open("{}")
    panel.openImagePreview(fixturePath("pixel.svg"), "1")
    var viewer = control("imageViewerPopup")
    tryCompare(viewer, "visible", true)
    compare(viewer.width, viewer.parent.width)
    compare(viewer.height, viewer.parent.height)
    compare(viewer.modal, true)
    panel.closeImagePreview()
    tryCompare(viewer, "visible", false)
    tryCompare(panel, "imagePreviewUrl", "")
  }

  function test_image_caption_sits_below_preview() {
    panel.open('{"chatJid":"alice@s.whatsapp.net"}')
    var imagePath = fixturePath("pixel.svg")
    service.loadMessages([{
      id: "captioned-image",
      chat_jid: "alice@s.whatsapp.net",
      sender_jid: "me",
      from_me: true,
      text: "Hi",
      timestamp: 200,
      media: {
        kind: "image",
        path: imagePath,
        thumbnail_path: imagePath,
        downloaded: true,
        width: 16,
        height: 9
      }
    }], "")
    tryCompare(control("messageList"), "count", 1)
    var delegate = control("messageDelegate-captioned-image")
    var bubble = control("messageBubble-captioned-image")
    var caption = control("mediaCaptionText-captioned-image")
    var mediaCard = control("mediaPreviewCard-captioned-image")
    var imageMask = control("mediaPreviewMask-captioned-image")
    var timestamp = control("messageTimestamp-captioned-image")
    compare(bubble.showMessageBubble, true)
    verify(bubble.height > 0)
    compare(caption.text, "Hi")
    compare(caption.parent, control("messageColumn-captioned-image"))
    verify(caption.height > 0)
    verify(bubble.height >= caption.height)
    compare(findChild(bubble, "mediaPreviewCard-captioned-image"), null)
    compare(mediaCard.topMargin, 0)
    compare(imageMask.radius, bubble.radius)
    compare(mediaCard.y, control("dateDivider-captioned-image").height)
    compare(bubble.y, mediaCard.y + mediaCard.height + Style.space(8))
    verify(bubble.width < mediaCard.width)
    compare(bubble.x + bubble.width, delegate.width - Style.space(18))
    compare(mediaCard.x + mediaCard.width, delegate.width - Style.space(18))
    verify(mappedBottom(bubble, delegate)
      <= timestamp.parent.mapToItem(delegate, 0, 0).y)
  }

  function test_group_image_keeps_sender_header_above_preview() {
    panel.open('{"chatJid":"team@g.us"}')
    var imagePath = fixturePath("pixel.svg")
    service.loadMessages([{
      id: "group-image",
      chat_jid: "team@g.us",
      sender_jid: "alice@s.whatsapp.net",
      sender_name: "Alice",
      from_me: false,
      text: "[Image]",
      timestamp: 200,
      media: {
        kind: "image",
        path: imagePath,
        thumbnail_path: imagePath,
        downloaded: true,
        width: 1,
        height: 1
      }
    }], "")
    tryCompare(control("messageList"), "count", 1)
    var bubble = control("messageBubble-group-image")
    var header = control("senderHeader-group-image")
    var mediaCard = control("mediaPreviewCard-group-image")
    var imageMask = control("mediaPreviewMask-group-image")
    compare(header.text, "Alice")
    verify(header.height > 0)
    compare(header.y, control("dateDivider-group-image").height)
    compare(header.x, Style.space(56))
    compare(bubble.showMessageBubble, false)
    compare(bubble.height, 0)
    compare(findChild(bubble, "mediaPreviewCard-group-image"), null)
    compare(imageMask.radius, Style.cornerRadius + Style.space(6))
    compare(mediaCard.y, header.y + header.height + Style.space(4))
    compare(mediaCard.x, Style.space(56))
    verify(mediaCard.width > header.width)
  }

  function test_video_preview_without_caption_has_no_bubble_data() {
    return [
      { tag: "video", gif: false, downloaded: true, tooltip: "Play video" },
      { tag: "gif", gif: true, downloaded: true, tooltip: "Play GIF" },
      { tag: "pending-download", gif: false, downloaded: false, tooltip: "Download video" }
    ]
  }

  function test_video_preview_without_caption_has_no_bubble(data) {
    panel.open('{"chatJid":"alice@s.whatsapp.net"}')
    var previewPath = fixturePath("pixel.svg")
    var media = {
      kind: "video",
      path: "/synthetic/media/clip.mp4",
      thumbnail_path: previewPath,
      downloaded: data.downloaded,
      width: 16,
      height: 9
    }
    if (data.gif) media.gif_playback = true
    service.loadMessages([{
      id: "video-style",
      chat_jid: "alice@s.whatsapp.net",
      sender_jid: "me",
      from_me: true,
      text: "[Video]",
      timestamp: 200,
      media: media
    }], "")
    tryCompare(control("messageList"), "count", 1)
    var delegate = control("messageDelegate-video-style")
    var bubble = control("messageBubble-video-style")
    var mediaCard = control("mediaPreviewCard-video-style")
    var previewImage = control("mediaPreviewImage-video-style")
    var downloadButton = control("mediaDownloadButton-video-style")
    var timestamp = control("messageTimestamp-video-style")
    compare(delegate.hasMediaPreview, true)
    compare(bubble.showMessageBubble, false)
    compare(bubble.height, 0)
    compare(findChild(bubble, "mediaPreviewCard-video-style"), null)
    compare(mediaCard.topMargin, 0)
    compare(mediaCard.isVideo, true)
    compare(mediaCard.isGif, data.gif)
    verify(mediaCard.height > 0)
    compare(previewImage.y, 0)
    compare(previewImage.height, mediaCard.height)
    compare(previewImage.layer.enabled, true)
    compare(control("mediaPreviewMask-video-style").radius,
      Style.cornerRadius + Style.space(6))
    compare(downloadButton.tooltipText, data.tooltip)
    // Centered anchors snap to whole pixels, so fractional card heights
    // leave the button up to half a pixel off the exact center.
    verify(Math.abs(downloadButton.y + downloadButton.height / 2
      - mediaCard.height / 2) <= 0.5)
    compare(mediaCard.x + mediaCard.width, delegate.width - Style.space(18))
    verify(mappedBottom(mediaCard, delegate)
      <= timestamp.parent.mapToItem(delegate, 0, 0).y)
  }

  function test_video_caption_sits_below_preview() {
    panel.open('{"chatJid":"alice@s.whatsapp.net"}')
    var previewPath = fixturePath("pixel.svg")
    service.loadMessages([{
      id: "captioned-video",
      chat_jid: "alice@s.whatsapp.net",
      sender_jid: "me",
      from_me: true,
      text: "Watch this",
      timestamp: 200,
      media: {
        kind: "video",
        path: "/synthetic/media/clip.mp4",
        thumbnail_path: previewPath,
        downloaded: true,
        width: 16,
        height: 9
      }
    }], "")
    tryCompare(control("messageList"), "count", 1)
    var delegate = control("messageDelegate-captioned-video")
    var bubble = control("messageBubble-captioned-video")
    var caption = control("mediaCaptionText-captioned-video")
    var mediaCard = control("mediaPreviewCard-captioned-video")
    var imageMask = control("mediaPreviewMask-captioned-video")
    var timestamp = control("messageTimestamp-captioned-video")
    compare(bubble.showMessageBubble, true)
    verify(bubble.height > 0)
    compare(caption.text, "Watch this")
    compare(caption.parent, control("messageColumn-captioned-video"))
    verify(caption.height > 0)
    verify(bubble.height >= caption.height)
    compare(findChild(bubble, "mediaPreviewCard-captioned-video"), null)
    compare(mediaCard.topMargin, 0)
    compare(imageMask.radius, bubble.radius)
    compare(mediaCard.y, control("dateDivider-captioned-video").height)
    compare(bubble.y, mediaCard.y + mediaCard.height + Style.space(8))
    verify(bubble.width < mediaCard.width)
    compare(bubble.x + bubble.width, delegate.width - Style.space(18))
    compare(mediaCard.x + mediaCard.width, delegate.width - Style.space(18))
    verify(mappedBottom(bubble, delegate)
      <= timestamp.parent.mapToItem(delegate, 0, 0).y)
  }

  function test_group_video_keeps_sender_header_above_preview() {
    panel.open('{"chatJid":"team@g.us"}')
    var previewPath = fixturePath("pixel.svg")
    service.loadMessages([{
      id: "group-video",
      chat_jid: "team@g.us",
      sender_jid: "alice@s.whatsapp.net",
      sender_name: "Alice",
      from_me: false,
      text: "[Video]",
      timestamp: 200,
      media: {
        kind: "video",
        path: "/synthetic/media/clip.mp4",
        thumbnail_path: previewPath,
        downloaded: true,
        width: 1,
        height: 1
      }
    }], "")
    tryCompare(control("messageList"), "count", 1)
    var bubble = control("messageBubble-group-video")
    var header = control("senderHeader-group-video")
    var mediaCard = control("mediaPreviewCard-group-video")
    var imageMask = control("mediaPreviewMask-group-video")
    compare(header.text, "Alice")
    verify(header.height > 0)
    compare(header.y, control("dateDivider-group-video").height)
    compare(header.x, Style.space(56))
    compare(bubble.showMessageBubble, false)
    compare(bubble.height, 0)
    compare(findChild(bubble, "mediaPreviewCard-group-video"), null)
    compare(imageMask.radius, Style.cornerRadius + Style.space(6))
    compare(mediaCard.y, header.y + header.height + Style.space(4))
    compare(mediaCard.x, Style.space(56))
    verify(mediaCard.width > header.width)
  }

  function test_video_click_opens_original_size_viewer_data() {
    return [
      { tag: "video", gif: false, playTooltip: "Play video", pauseTooltip: "Pause video" },
      { tag: "gif", gif: true, playTooltip: "Play GIF", pauseTooltip: "Pause GIF" }
    ]
  }

  function test_video_click_opens_original_size_viewer(data) {
    panel.open('{"chatJid":"alice@s.whatsapp.net"}')
    var previewPath = fixturePath("pixel.svg")
    var media = {
      kind: "video",
      path: "/synthetic/media/clip.mp4",
      thumbnail_path: previewPath,
      downloaded: true,
      width: 16,
      height: 9
    }
    if (data.gif) media.gif_playback = true
    service.loadMessages([{
      id: "video-viewer",
      chat_jid: "alice@s.whatsapp.net",
      sender_jid: "me",
      from_me: true,
      text: "[Video]",
      timestamp: 200,
      media: media
    }], "")
    tryCompare(control("messageList"), "count", 1)
    control("mediaPreviewCard-video-viewer").openPreview()
    var popup = control("videoPreviewPopup")
    tryCompare(popup, "opened", true)
    compare(panel.videoPreviewIsGif, data.gif)
    verify(panel.videoPreviewUrl.endsWith("/synthetic/media/clip.mp4"))
    compare(panel.videoPreviewUrl.indexOf("?"), -1)
    compare(control("fullVideoPreview").fillMode, VideoOutput.PreserveAspectFit)
    compare(control("videoPreviewStatus").visible, false)
    compare(control("videoPreviewTimeLabel").text, "0:00 / 0:00")
    compare(control("videoPreviewSeekFill").width, 0)
    var playButton = control("videoPreviewPlayButton")
    compare(playButton.tooltipText, data.pauseTooltip)
    playButton.click()
    compare(playButton.tooltipText, data.playTooltip)
    playButton.click()
    compare(playButton.tooltipText, data.pauseTooltip)
    tryCompare(popup, "opened", true)
    verify(panel.videoPreviewUrl.endsWith("/synthetic/media/clip.mp4"))

    control("videoPreviewCloseButton").click()
    tryCompare(popup, "opened", false)
    tryCompare(panel, "videoPreviewUrl", "")
    tryCompare(panel, "videoPreviewIsGif", false)

    control("mediaPreviewCard-video-viewer").openPreview()
    tryCompare(popup, "opened", true)
    panel.chooseChat("team@g.us")
    tryCompare(popup, "opened", false)
    tryCompare(panel, "videoPreviewUrl", "")
  }

  function albumMessage(id, options) {
    options = options || {}
    var kind = options.kind || "image"
    var message = {
      id: id,
      chat_jid: "alice@s.whatsapp.net",
      sender_jid: options.sender || "alice@s.whatsapp.net",
      sender_name: options.senderName || "Alice",
      from_me: false,
      text: options.text !== undefined ? options.text
        : (kind === "video" ? "[Video]" : "[Image]"),
      timestamp: options.timestamp !== undefined ? options.timestamp : 200,
      media: {
        kind: kind,
        path: kind === "video" ? "/synthetic/media/clip.mp4"
          : fixturePath("pixel.svg"),
        thumbnail_path: fixturePath("pixel.svg"),
        downloaded: true,
        width: 16,
        height: 9
      }
    }
    if (options.reactions) message.reactions = options.reactions
    return message
  }

  function test_album_groups_consecutive_uncaptioned_media_data() {
    return [
      {
        tag: "two-images",
        messages: [albumMessage("m0"), albumMessage("m1")],
        mosaicLeader: "m0",
        followers: ["m1"],
        standalone: []
      },
      {
        tag: "image-video-mix",
        messages: [albumMessage("m0"), albumMessage("m1", { kind: "video" })],
        mosaicLeader: "m0",
        followers: ["m1"],
        standalone: []
      },
      {
        tag: "caption-breaks-run",
        messages: [albumMessage("m0"), albumMessage("m1", { text: "Nice!" })],
        mosaicLeader: null,
        followers: [],
        standalone: ["m0", "m1"]
      },
      {
        tag: "reaction-breaks-run",
        messages: [albumMessage("m0"),
          albumMessage("m1", { reactions: [{ emoji: "👍", from_me: false, count: 1 }] })],
        mosaicLeader: null,
        followers: [],
        standalone: ["m0", "m1"]
      },
      {
        tag: "different-minute-breaks-run",
        messages: [albumMessage("m0", { timestamp: 200 }),
          albumMessage("m1", { timestamp: 320 })],
        mosaicLeader: null,
        followers: [],
        standalone: ["m0", "m1"]
      },
      {
        tag: "text-between-breaks-run",
        messages: [albumMessage("m0"),
          { id: "m1", chat_jid: "alice@s.whatsapp.net", sender_jid: "alice@s.whatsapp.net", sender_name: "Alice", from_me: false, text: "hello", timestamp: 201 },
          albumMessage("m2")],
        mosaicLeader: null,
        followers: [],
        standalone: ["m0", "m2"]
      },
      {
        tag: "different-sender-breaks-run",
        messages: [albumMessage("m0"),
          albumMessage("m1", { sender: "bob@s.whatsapp.net", senderName: "Bob" })],
        mosaicLeader: null,
        followers: [],
        standalone: ["m0", "m1"]
      },
      {
        tag: "single-never-groups",
        messages: [albumMessage("m0")],
        mosaicLeader: null,
        followers: [],
        standalone: ["m0"]
      },
      {
        tag: "five-chunks-four-plus-single",
        messages: [albumMessage("m0"), albumMessage("m1"),
          albumMessage("m2"), albumMessage("m3"), albumMessage("m4")],
        mosaicLeader: "m0",
        followers: ["m1", "m2", "m3"],
        standalone: ["m4"]
      }
    ]
  }

  function test_album_groups_consecutive_uncaptioned_media(data) {
    panel.open('{"chatJid":"alice@s.whatsapp.net"}')
    service.loadMessages(data.messages, "")
    tryCompare(control("messageList"), "count", data.messages.length)
    var mosaic = findChild(panel, "albumMosaic-" + data.messages[0].id)
    if (data.mosaicLeader) {
      compare(data.messages[0].id, data.mosaicLeader)
      verify(mosaic !== null)
      verify(mosaic.height > 0)
      for (var f = 0; f < data.followers.length; f++) {
        var followerId = data.followers[f]
        compare(findChild(panel, "messageBubble-" + followerId).height, 0)
        compare(findChild(panel, "mediaPreviewCard-" + followerId), null)
        compare(findChild(panel, "messageDelegate-" + followerId).height, 0)
      }
    } else {
      compare(mosaic, null)
      for (var m = 0; m < data.messages.length; m++)
        compare(findChild(panel, "albumMosaic-" + data.messages[m].id), null)
    }
    for (var s = 0; s < data.standalone.length; s++) {
      var standaloneId = data.standalone[s]
      verify(findChild(panel, "mediaPreviewCard-" + standaloneId) !== null)
      verify(findChild(panel, "albumTile-" + standaloneId) === null)
    }
  }

  function test_album_footer_shows_last_message_time() {
    panel.open('{"chatJid":"alice@s.whatsapp.net"}')
    function outgoingImage(id, timestamp, receipt, readBy) {
      return {
        id: id,
        chat_jid: "alice@s.whatsapp.net",
        sender_jid: "me",
        from_me: true,
        text: "[Image]",
        timestamp: timestamp,
        receipt: receipt,
        read_by: readBy || [],
        media: {
          kind: "image",
          path: fixturePath("pixel.svg"),
          thumbnail_path: fixturePath("pixel.svg"),
          downloaded: true,
          width: 16,
          height: 9
        }
      }
    }
    service.loadMessages([
      outgoingImage("a0", 200, 1),
      outgoingImage("a1", 201, 1),
      outgoingImage("a2", 202, 3, [
        { jid: "alice@s.whatsapp.net", name: "Alice", read_at: 203 }
      ])
    ], "")
    tryCompare(control("messageList"), "count", 3)
    verify(findChild(panel, "albumMosaic-a0") !== null)
    compare(control("messageTimestamp-a0").text,
      Model.messageTime(202, panel.messageTimeFormat))
    compare(control("messageReceiptStatus-a0").text, "✓✓")
    verify(control("messageReceiptStatus-a0").receiptTooltipText
      .indexOf("Alice") >= 0)
  }

  function test_group_album_shows_single_sender_header_and_avatar() {
    panel.open('{"chatJid":"team@g.us"}')
    function groupImage(id, timestamp) {
      return {
        id: id,
        chat_jid: "team@g.us",
        sender_jid: "alice@s.whatsapp.net",
        sender_name: "Alice",
        from_me: false,
        text: "[Image]",
        timestamp: timestamp,
        media: {
          kind: "image",
          path: fixturePath("pixel.svg"),
          thumbnail_path: fixturePath("pixel.svg"),
          downloaded: true,
          width: 1,
          height: 1
        }
      }
    }
    service.loadMessages([groupImage("g0", 200), groupImage("g1", 201)], "")
    tryCompare(control("messageList"), "count", 2)
    var mosaic = control("albumMosaic-g0")
    var header = control("senderHeader-g0")
    compare(header.text, "Alice")
    verify(header.height > 0)
    compare(header.x, Style.space(56))
    compare(control("messageBubble-g0").showMessageBubble, false)
    compare(control("messageBubble-g0").height, 0)
    compare(control("messageBubble-g1").height, 0)
    compare(control("senderAvatar-g0").width, Style.space(30))
    compare(control("senderAvatar-g1").width, 0)
    compare(mosaic.y, header.y + header.height + Style.space(4))
    compare(mosaic.x, Style.space(56))
    compare(control("messageTimestamp-g0").text,
      Model.messageTime(201, panel.messageTimeFormat))
    verify(mappedBottom(mosaic, control("messageDelegate-g0"))
      <= control("messageTimestamp-g0").parent
        .mapToItem(control("messageDelegate-g0"), 0, 0).y)
  }

  function test_album_tile_download_and_open() {
    panel.open('{"chatJid":"alice@s.whatsapp.net"}')
    service.loadMessages([
      {
        id: "am0",
        chat_jid: "alice@s.whatsapp.net",
        sender_jid: "alice@s.whatsapp.net",
        sender_name: "Alice",
        from_me: false,
        text: "[Image]",
        timestamp: 200,
        media: {
          kind: "image",
          path: "",
          thumbnail_path: fixturePath("pixel.svg"),
          downloaded: false,
          width: 16,
          height: 9
        }
      },
      albumMessage("am1", { kind: "video" }),
      albumMessage("am2")
    ], "")
    tryCompare(control("messageList"), "count", 3)
    var mosaic = control("albumMosaic-am0")
    control("albumDownloadButton-am0").click()
    compare(service.downloadedMessages.length, 1)
    compare(service.downloadedMessages[0].id, "am0")

    mosaic.openTile(service.messages[1])
    tryCompare(control("videoPreviewPopup"), "opened", true)
    verify(panel.videoPreviewUrl.endsWith("/synthetic/media/clip.mp4"))
    control("videoPreviewCloseButton").click()
    tryCompare(control("videoPreviewPopup"), "opened", false)

    mosaic.openTile(service.messages[2])
    verify(panel.imagePreviewUrl.endsWith("/fixtures/pixel.svg?v=0-0"))
  }

  function test_hd_badge_marks_high_definition_images() {
    panel.open('{"chatJid":"alice@s.whatsapp.net"}')
    function definitionImage(id, timestamp, width, height) {
      return {
        id: id,
        chat_jid: "alice@s.whatsapp.net",
        sender_jid: "alice@s.whatsapp.net",
        sender_name: "Alice",
        from_me: false,
        text: "[Image]",
        timestamp: timestamp,
        media: {
          kind: "image",
          path: fixturePath("pixel.svg"),
          thumbnail_path: fixturePath("pixel.svg"),
          downloaded: true,
          width: width,
          height: height
        }
      }
    }
    service.loadMessages([
      definitionImage("hd-photo", 200, 4000, 3000),
      definitionImage("sd-photo", 320, 800, 600)
    ], "")
    tryCompare(control("messageList"), "count", 2)
    var hdCard = control("mediaPreviewCard-hd-photo")
    compare(hdCard.showHdBadge, true)
    verify(findChild(hdCard, "hdBadge-hd-photo") !== null)
    compare(control("mediaPreviewCard-sd-photo").showHdBadge, false)
  }

  function test_paste_image_stage_send_and_discard() {
    panel.open('{"chatJid":"alice@s.whatsapp.net"}')
    var composer = control("composer")
    compare(composer.placeholderText, "Message")
    compare(control("sendButton").tooltipText, "Send message")
    var plainWidth = composer.width

    panel.pasteImageFromClipboard()
    compare(service.stagedImage !== null, true)
    compare(composer.placeholderText, "Add a caption")
    compare(control("sendButton").tooltipText, "Send image")
    verify(composer.width < plainWidth)
    verify(control("pastedThumbnail").source.toString()
      .endsWith("/fixtures/pixel.svg?v=0"))

    composer.text = "look at this"
    control("sendButton").click()
    compare(service.sentImages.length, 1)
    compare(service.sentImages[0].text, "look at this")
    compare(service.sentImages[0].chat_jid, "alice@s.whatsapp.net")
    compare(service.stagedImage, null)
    compare(composer.text, "")
    compare(composer.placeholderText, "Message")
    compare(control("sendButton").tooltipText, "Send message")

    panel.pasteImageFromClipboard()
    compare(service.stagedImage !== null, true)
    composer.text = "keep me"
    control("discardStagedButton").click()
    compare(service.stagedImage, null)
    compare(composer.text, "keep me")

    service.pasteImageEmpty = true
    var pasted = []
    service.clipboardTextPasteRequested.connect(function () {
      pasted.push(true)
    })
    panel.pasteImageFromClipboard()
    compare(pasted.length, 1)
    compare(service.stagedImage, null)
  }

  function test_render_and_download_stickers() {
    panel.open('{"chatJid":"alice@s.whatsapp.net"}')
    var previewPath = String(Qt.resolvedUrl("fixtures/pixel.svg"))
    previewPath = decodeURIComponent(previewPath.substring("file://".length))
    service.loadMessages([{
      id: "sticker-1",
      chat_jid: "alice@s.whatsapp.net",
      sender_jid: "alice@s.whatsapp.net",
      sender_name: "Alice",
      text: "[Sticker]",
      timestamp: 100,
      media: {
        kind: "sticker",
        path: "/private/sticker.webp",
        thumbnail_path: previewPath,
        downloaded: false,
        mime_type: "image/webp",
        width: 512,
        height: 512,
        animated: true,
        lottie: false,
        accessibility_label: "Dancing parrot"
      }
    }], "")
    tryCompare(control("messageList"), "count", 1)
    var stickerDelegate = control("messageDelegate-sticker-1")
    compare(stickerDelegate.isSticker, true)
    compare(control("stickerCard-sticker-1").active, true)
    verify(String(control("stickerImage-sticker-1").source)
      .indexOf("/fixtures/pixel.svg") >= 0)
    compare(findChild(panel, "stickerDownloadButton-sticker-1"), null)
    compare(service.downloadedMessages.length, 1)
    compare(service.downloadedMessages[0].id, "sticker-1")
    compare(control("stickerDownloadStatus-sticker-1").active, true)

    service.loadMessages([{
      id: "lottie-1",
      chat_jid: "alice@s.whatsapp.net",
      sender_jid: "alice@s.whatsapp.net",
      sender_name: "Alice",
      text: "[Sticker]",
      timestamp: 101,
      media: {
        kind: "sticker",
        path: "",
        thumbnail_path: previewPath,
        downloaded: false,
        mime_type: "application/json",
        width: 512,
        height: 512,
        animated: true,
        lottie: true,
        accessibility_label: "Waving hand"
      }
    }], "")
    tryCompare(control("messageList"), "count", 1)
    tryCompare(control("stickerCard-lottie-1"), "lottie", true)
    compare(findChild(panel, "stickerDownloadButton-lottie-1"), null)
    compare(service.downloadedMessages.length, 1)
    compare(control("stickerDownloadStatus-lottie-1").active, false)
  }

  function test_unread_filter_preserves_selected_chat() {
    service.selectedChatJid = "team@g.us"
    service.unreadOnly = true
    tryCompare(panel.filteredChats, "length", 2)
    service.selectedChatJid = ""
    tryCompare(panel.filteredChats, "length", 1)
    compare(panel.filteredChats[0].jid, "alice@s.whatsapp.net")
  }

  function test_search_ignores_unread_filter() {
    service.selectedChatJid = ""
    service.unreadOnly = true
    tryCompare(panel.filteredChats, "length", 1)
    control("chatSearch").text = "team"
    tryCompare(panel.filteredChats, "length", 1)
    compare(panel.filteredChats[0].jid, "team@g.us")
    control("chatSearch").text = ""
    tryCompare(panel.filteredChats, "length", 1)
    compare(panel.filteredChats[0].jid, "alice@s.whatsapp.net")
  }

  function test_search_clear_button() {
    panel.open("{}")
    var search = control("chatSearch")
    var clear = control("chatSearchClear")
    compare(clear.enabled, false)
    compare(search.rightPadding, search.leftPadding)
    search.text = "team"
    compare(clear.enabled, true)
    compare(search.rightPadding, search.height)
    clear.clicked(null)
    compare(search.text, "")
    compare(clear.enabled, false)
  }

  function test_sidebar_arrow_keys_navigate_chats() {
    panel.open("{}")
    service.selectedChatJid = "alice@s.whatsapp.net"
    verify(panel.handleChatListKey(Qt.Key_Down))
    compare(service.selectedChatJid, "team@g.us")
    verify(panel.handleChatListKey(Qt.Key_Up))
    compare(service.selectedChatJid, "alice@s.whatsapp.net")
    verify(panel.handleChatListKey(Qt.Key_Up))
    compare(service.selectedChatJid, "alice@s.whatsapp.net")
    verify(panel.handleChatListKey(Qt.Key_Right))
    verify(!panel.handleChatListKey(Qt.Key_Left))
    verify(!panel.handleChatListKey(Qt.Key_A))
    compare(service.selectedChatJid, "alice@s.whatsapp.net")
  }

  function test_control_shortcuts_toggle_unread_and_search() {
    panel.open("{}")
    compare(panel.handleControlShortcut({ key: Qt.Key_U, modifiers: Qt.NoModifier }), false)
    verify(panel.handleControlShortcut({ key: Qt.Key_U, modifiers: Qt.ControlModifier }))
    compare(service.unreadOnly, true)
    verify(panel.handleControlShortcut({ key: Qt.Key_U, modifiers: Qt.ControlModifier }))
    compare(service.unreadOnly, false)
    verify(panel.handleControlShortcut({ key: Qt.Key_F, modifiers: Qt.ControlModifier }))
    verify(panel.handleControlShortcut({ key: Qt.Key_L, modifiers: Qt.ControlModifier }))
    compare(panel.handleControlShortcut({ key: Qt.Key_A, modifiers: Qt.ControlModifier }), false)
  }

  function test_search_down_selects_first_result() {
    panel.open("{}")
    service.selectedChatJid = ""
    control("chatSearch").text = "release"
    verify(panel.focusFirstChatResult())
    compare(service.selectedChatJid, "team@g.us")
    control("chatSearch").text = "nobody"
    tryCompare(panel.filteredChats, "length", 0)
    compare(panel.focusFirstChatResult(), false)
  }

  function test_shortcuts_overlay_opens_from_keyboard_and_menu() {
    panel.open("{}")
    var popup = panel.shortcutsViewer
    compare(popup.opened, false)
    verify(panel.handleControlShortcut({
      key: Qt.Key_Question, modifiers: Qt.ControlModifier | Qt.ShiftModifier }))
    tryCompare(popup, "opened", true)
    verify(popup.sections.length > 0)
    popup.close()
    tryCompare(popup, "opened", false)

    control("headerMoreButton").click()
    tryCompare(panel.appMenu, "opened", true)
    control("headerShortcutsAction").click()
    tryCompare(popup, "opened", true)
    popup.close()
  }

  function test_composer_grows_and_shift_enter_adds_newline() {
    panel.open('{"chatJid":"alice@s.whatsapp.net"}')
    var composer = control("composer")
    var scroll = control("composerScroll")
    var sendButton = control("sendButton")
    var singleLine = sendButton.height
    compare(scroll.height, singleLine)

    composer.text = "one\ntwo\nthree"
    tryVerify(function() { return scroll.height > singleLine })
    compare(sendButton.height, singleLine)
    composer.text = new Array(40).join("line\n")
    tryCompare(scroll, "height", singleLine * 6)

    var shiftEnter = { modifiers: Qt.ShiftModifier, accepted: true }
    composer.handleReturn(shiftEnter)
    compare(shiftEnter.accepted, false)
    compare(service.sentMessages.length, 0)

    composer.text = "first\nsecond"
    composer.handleReturn({ modifiers: Qt.NoModifier, accepted: true })
    compare(service.sentMessages.length, 1)
    compare(service.sentMessages[0], "first\nsecond")
    tryCompare(scroll, "height", singleLine)
  }
}
