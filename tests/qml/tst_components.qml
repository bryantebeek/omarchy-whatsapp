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
