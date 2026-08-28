import QtQuick
import QtTest
import Quickshell

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

  function test_open_search_select_and_close() {
    panel.open('{"chatJid":"team@g.us"}')
    compare(panel.opened, true)
    compare(service.selectedChatJid, "team@g.us")
    verify(callRecorded("refreshMetadata"))
    verify(callRecorded("setPanelState"))

    var search = control("chatSearch")
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

  function test_new_conversation_and_send_message() {
    panel.open("{}")
    var newChat = control("newChat")
    panel.newChatVisible = true
    newChat.text = "+31 (6) 1234"
    control("openChatButton").click()
    compare(service.selectedChatJid, "3161234@s.whatsapp.net")
    compare(panel.newChatVisible, false)
    compare(newChat.text, "")

    var composer = control("composer")
    composer.text = "Hello from the test"
    control("sendButton").click()
    compare(service.sentMessages.length, 1)
    compare(service.sentMessages[0], "Hello from the test")
    compare(composer.text, "")
    wait(10)
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
      { id: "m2", chat_jid: "alice@s.whatsapp.net", sender_jid: "me", from_me: true, text: "Second", timestamp: 101, receipt: 3 }
    ], "m1")
    tryCompare(control("messageList"), "count", 2)
    var receiptStatus = control("messageReceiptStatus-m2")
    compare(receiptStatus.text, "✓✓")
    compare(receiptStatus.color, panel.accent)
    compare(receiptStatus.font.pixelSize, 12)
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

  function test_unread_filter_preserves_selected_chat() {
    service.selectedChatJid = "team@g.us"
    service.unreadOnly = true
    tryCompare(panel.filteredChats, "length", 2)
    service.selectedChatJid = ""
    tryCompare(panel.filteredChats, "length", 1)
    compare(panel.filteredChats[0].jid, "alice@s.whatsapp.net")
  }
}
