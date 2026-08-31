import QtQuick
import QtTest
import Quickshell
import qs.Commons

import "../../quickshell" as Whatsapp
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
    for (var i = 0; i < service.calls.length; i++)
      if (service.calls[i].name === name) return true
    return false
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
    compare(control("composer").enabled, true)
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
        total_voters: 1,
        end_timestamp: 0,
        options: [
          { name: "Soup", votes: 1, selected_by_me: false },
          { name: "Salad", votes: 0, selected_by_me: false }
        ]
      }
    }], "")
    tryCompare(control("messageList"), "count", 1)
    var pollDelegate = control("messageDelegate-poll-1")
    compare(pollDelegate.mediaData.kind, "poll")
    compare(pollDelegate.isPoll, true)
    verify(control("pollCard-poll-1") !== null)
    pollDelegate.togglePollOption(0)
    compare(service.pollVotes.length, 1)
    compare(service.pollVotes[0].selectedOptions[0], "Soup")
  }

  function test_open_media_and_recover_connection() {
    panel.open('{"chatJid":"alice@s.whatsapp.net"}')
    var imagePath = String(Qt.resolvedUrl("fixtures/pixel.svg"))
    imagePath = decodeURIComponent(imagePath.substring("file://".length))
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
    var textBubble = control("messageBubble-text-style")
    var imageBubble = control("messageBubble-image-style")
    compare(imageBubble.borderOnlyMedia, false)
    compare(imageBubble.radius, textBubble.radius)
    compare(imageBubble.color, textBubble.color)
    compare(imageBubble.borderLeft, textBubble.borderLeft)
    compare(imageBubble.borderTop, textBubble.borderTop)
    compare(imageBubble.horizontalPadding, textBubble.horizontalPadding)
    var mediaCard = control("mediaPreviewCard-image-style")
    var imageMask = control("mediaPreviewMask-image-style")
    var previewImage = control("mediaPreviewImage-image-style")
    var downloadButton = control("mediaDownloadButton-image-style")
    compare(mediaCard.topMargin, Style.space(8))
    compare(previewImage.y, mediaCard.topMargin)
    compare(previewImage.height, mediaCard.height - mediaCard.topMargin)
    compare(imageMask.radius, imageBubble.radius)
    compare(imageMask.y, mediaCard.topMargin)
    compare(previewImage.layer.enabled, true)
    compare(downloadButton.y + downloadButton.height / 2,
      mediaCard.topMargin
        + (mediaCard.height - mediaCard.topMargin) / 2)

    panel.openImagePreview(imagePath, "7")
    verify(panel.imagePreviewUrl.endsWith("/fixtures/pixel.svg?v=7"))

    service.connectionState = "disconnected"
    tryCompare(panel, "paired", false)
    service.lastError = "Network unavailable"
    compare(service.lastError, "Network unavailable")
    service.connectionState = "connected"
    service.lastError = ""
    tryCompare(panel, "paired", true)
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
}
