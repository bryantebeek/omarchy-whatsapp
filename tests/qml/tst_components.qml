import QtQuick
import QtTest

import "../../quickshell" as Whatsapp
import "fixtures"

TestCase {
  id: testCase
  name: "QmlComponents"

  Component {
    id: panelComponent
    Whatsapp.Panel {}
  }

  Component {
    id: barComponent
    Whatsapp.BarWidget {}
  }

  Component { id: serviceComponent; WorkflowService {} }

  function init() {
    failOnWarning(/.*/)
  }

  function test_panel_loads() {
    var service = createTemporaryObject(serviceComponent, testCase)
    service.chats = [{ jid: "group@g.us", name: "Test Group", is_group: true }]
    service.selectedChatJid = "group@g.us"
    service.groupParticipantsChatJid = "group@g.us"
    service.groupParticipants = [
      { jid: "me", name: "", is_me: true },
      { jid: "alice", name: "Alice" },
      { jid: "bob", name: "Bob" },
      { jid: "carol", name: "Carol" },
      { jid: "dave", name: "Dave" }
    ]
    var panel = createTemporaryObject(panelComponent, testCase, { service: service })
    verify(panel !== null)
    compare(panel.groupConversationSubtitle(), "You, Alice, Bob, Carol + 1 other")
    compare(panel.conversationActivitySubtitle(),
      "You, Alice, Bob, Carol + 1 other")
    service.chatStateLabelResult = "Alice is typing…"
    compare(panel.conversationChatActivity(), "Alice is typing…")
    compare(panel.conversationActivitySubtitle(), "Alice is typing…")
    compare(panel.sidebarChatActivity(service.selectedChat), "Alice is typing…")
    service.chatStateLabelResult = ""
    compare(panel.conversationChatActivity(), "")
    compare(panel.sidebarChatActivity(service.selectedChat), "")
    service.groupParticipantsError = "failed"
    compare(panel.groupConversationSubtitle(), "Participants unavailable")
    compare(panel.licenseKindLabel("project"), "Application")
    compare(panel.licenseKindLabel("asset"), "Bundled asset")
    compare(panel.licenseKindLabel("crate"), "Rust package")
    panel.licenseEntries = [
      { name: "Alpha", version: "1.0", license: "MIT" },
      { name: "Beta", version: "2.0", license: "Apache-2.0" }
    ]
    compare(panel.filteredLicenses("").length, 2)
    compare(panel.filteredLicenses("apache")[0].name, "Beta")
    compare(panel.chatShortcutSlot(Qt.Key_1), 0)
    compare(panel.chatShortcutSlot(Qt.Key_0), 9)
    compare(panel.chatShortcutSlot(Qt.Key_A), -1)
    compare(panel.remainingTimeLabel(panel.currentTimestamp + 65), "2 minutes left")
    compare(panel.messageReceiptIcon(0), "󰥔")
    compare(panel.messageReceiptIcon(1), "✓")
    compare(panel.messageReceiptIcon(3), "✓✓")
    compare(panel.messageReceiptIcon(4), "󰍬")
    compare(panel.messageReceiptLabel(3), "Read")
    compare(panel.messageReceiptTimestamp(0), "")
    compare(panel.messageReceiptTooltip({ receipt: 3, read_by: [] }), "Read")
    var delivered = panel.messageReceiptTimestamp(100)
    var read = panel.messageReceiptTimestamp(120)
    compare(panel.messageReceiptTooltip({
      receipt: 2,
      delivered_to: [{
        jid: "dave@s.whatsapp.net", name: "Dave", delivered_at: 100
      }]
    }), "Delivered\nDave · " + delivered)
    compare(panel.messageReceiptTooltip({
      receipt: 3,
      delivered_at: 100,
      read_at: 120,
      delivered_to: [
        { jid: "alice@s.whatsapp.net", name: "Alice", delivered_at: 100 }
      ],
      read_by: [
        { jid: "alice@s.whatsapp.net", name: "Alice", read_at: 120 }
      ]
    }), "Read\nAlice · " + read)
    compare(panel.messageReceiptTooltip({ receipt: 3, read_by: [
      { jid: "alice@s.whatsapp.net", name: "Alice" }
    ] }), "Read\nAlice")
    compare(panel.messageReceiptTooltip({ receipt: 3, read_by: [
      { jid: "bob@s.whatsapp.net", name: "Bob" },
      { jid: "alice@s.whatsapp.net", name: "Alice" },
      { jid: "alice@s.whatsapp.net", name: "Duplicate" }
    ] }), "Read\nAlice\nBob")
    var mixedGroups = panel.messageReceiptGroups({
      receipt: 3,
      delivered_to: [
        { jid: "alice@s.whatsapp.net", name: "Alice", delivered_at: 100 },
        { jid: "dave@s.whatsapp.net", name: "Dave", delivered_at: 100 },
        { jid: "dave@s.whatsapp.net", name: "Duplicate", delivered_at: 101 }
      ],
      read_by: [
        { jid: "alice@s.whatsapp.net", name: "Alice", read_at: 120 }
      ]
    })
    compare(mixedGroups.length, 2)
    compare(mixedGroups[0].label, "Read")
    compare(mixedGroups[0].entries[0], "Alice · " + read)
    compare(mixedGroups[1].label, "Delivered")
    compare(mixedGroups[1].entries[0], "Dave · " + delivered)
    compare(panel.messageReceiptTooltip({
      receipt: 3,
      delivered_to: [
        { jid: "alice@s.whatsapp.net", name: "Alice", delivered_at: 100 },
        { jid: "dave@s.whatsapp.net", name: "Dave", delivered_at: 100 }
      ],
      read_by: [
        { jid: "alice@s.whatsapp.net", name: "Alice", read_at: 120 }
      ]
    }), "Read\nAlice · " + read + "\n\nDelivered\nDave · " + delivered)

    service.chats = [{
      jid: "alice@s.whatsapp.net", name: "Alice", phone_number: "316123",
      is_group: false
    }]
    service.selectedChatJid = "alice@s.whatsapp.net"
    service.presenceLabelResult = "online"
    compare(panel.conversationActivitySubtitle(), "online")
    service.chatStateLabelResult = "typing…"
    compare(panel.sidebarChatActivity(service.selectedChat), "Typing…")
    service.chatStateLabelResult = ""
    service.presenceLabelResult = ""
    compare(panel.conversationActivitySubtitle(), "+316123")
  }

  function test_bar_widget_loads_and_formats_state() {
    var widget = createTemporaryObject(barComponent, testCase)
    verify(widget !== null)
    compare(widget.unread, 0)
    compare(widget.connectionState, "starting")
    compare(widget.showCount, true)
    compare(widget.hideWhenEmpty, false)
    compare(widget.unreadLabel, "0")

    widget.settings = { showUnreadCount: false, hideWhenEmpty: true }
    compare(widget.showCount, false)
    compare(widget.hideWhenEmpty, true)
  }
}
