import QtQuick
import QtTest

import "../../quickshell/Model.js" as Model

TestCase {
  name: "Model"

  function test_shallowCopy() {
    var original = { name: "Alice", nested: { value: 1 } }
    var copy = Model.shallowCopy(original)
    compare(copy.name, "Alice")
    verify(copy !== original)
    verify(copy.nested === original.nested)
    compare(Object.keys(Model.shallowCopy(null)).length, 0)
  }

  function test_normalizedJid_data() {
    return [
      { tag: "empty", input: null, expected: "" },
      { tag: "whitespace", input: "  ", expected: "" },
      { tag: "existing", input: " alice@g.us ", expected: "alice@g.us" },
      { tag: "formatted number", input: "+31 (6) 12-34", expected: "3161234@s.whatsapp.net" },
      { tag: "no digits", input: "Alice", expected: "" }
    ]
  }

  function test_normalizedJid(data) {
    compare(Model.normalizedJid(data.input), data.expected)
  }

  function test_friendlyName_data() {
    return [
      { tag: "real name", name: " Alice ", jid: "316@s.whatsapp.net", expected: "Alice" },
      { tag: "address as name", name: "316@s.whatsapp.net", jid: "316@s.whatsapp.net", expected: "+316" },
      { tag: "phone fallback", name: "", jid: "316@s.whatsapp.net", expected: "+316" },
      { tag: "group fallback", name: "", jid: "123456789@g.us", expected: "Group · 456789" },
      { tag: "lid name ignored", name: "Person@lid", jid: "123456@lid", expected: "Contact · 3456" },
      { tag: "unknown address", name: "", jid: "abc@example.test", expected: "abc@example.test" },
      { tag: "empty", name: null, jid: null, expected: "Conversation" }
    ]
  }

  function test_friendlyName(data) {
    compare(Model.friendlyName(data.name, data.jid), data.expected)
  }

  function test_contactPhoneNumber_data() {
    return [
      { tag: "explicit", phone: "+31 (6) 123", jid: "ignored@s.whatsapp.net", expected: "+316123" },
      { tag: "jid fallback", phone: "", jid: "316123@s.whatsapp.net", expected: "+316123" },
      { tag: "group has no fallback", phone: "", jid: "123@g.us", expected: "" },
      { tag: "no digits", phone: "private", jid: "", expected: "" }
    ]
  }

  function test_contactPhoneNumber(data) {
    compare(Model.contactPhoneNumber(data.phone, data.jid), data.expected)
  }

  function test_initials_data() {
    return [
      { tag: "two words", name: "Alice van Bob", jid: "", expected: "AB" },
      { tag: "one word", name: "alice", jid: "", expected: "AL" },
      { tag: "phone", name: "", jid: "31@s.whatsapp.net", expected: "31" },
      { tag: "empty fallback", name: "", jid: "", expected: "CO" }
    ]
  }

  function test_initials(data) {
    compare(Model.initials(data.name, data.jid), data.expected)
  }

  function test_unquotedDateFormat_data() {
    return [
      { tag: "plain", input: "HH:mm", expected: "HH:mm" },
      { tag: "quoted words", input: "ddd 'at' h:mm ap", expected: "ddd  h:mm ap" },
      { tag: "escaped quote", input: "HH''mm", expected: "HHmm" },
      { tag: "unterminated quote", input: "HH'ignored", expected: "HH" },
      { tag: "empty", input: null, expected: "" }
    ]
  }

  function test_unquotedDateFormat(data) {
    compare(Model.unquotedDateFormat(data.input), data.expected)
  }

  function test_timeFormat_data() {
    return [
      { tag: "24 hour", input: "dddd HH:mm", expected: "HH:mm" },
      { tag: "12 hour lowercase", input: "h:mm ap", expected: "h:mm ap" },
      { tag: "12 hour uppercase", input: "hh:m AP", expected: "hh:m AP" },
      { tag: "missing minute", input: "H", expected: "H:mm" },
      { tag: "skip invalid candidate", input: ["dddd", "hh:mm AP"], expected: "hh:mm AP" },
      { tag: "quoted hour ignored", input: "'HH' dddd", expected: "HH:mm" },
      { tag: "fallback", input: ["dddd", null], expected: "HH:mm" }
    ]
  }

  function test_timeFormat(data) {
    compare(Model.timeFormat(data.input), data.expected)
  }

  function test_shortTime() {
    compare(Model.shortTime(0, "HH:mm"), "")

    var now = new Date()
    var today = new Date(now.getFullYear(), now.getMonth(), now.getDate(), 13, 7, 0)
    compare(Model.shortTime(today.getTime() / 1000, "HH:mm"), Qt.formatTime(today, "HH:mm"))

    var recent = new Date(now.getTime() - 2 * 86400000)
    if (recent.toDateString() === now.toDateString())
      recent = new Date(now.getTime() - 3 * 86400000)
    var dutch = Qt.locale("nl_NL")
    var english = Qt.locale("en_US")
    compare(Model.shortTime(recent.getTime() / 1000, "HH:mm", dutch),
      recent.toLocaleDateString(dutch, "ddd"))
    compare(Model.shortTime(recent.getTime() / 1000, "HH:mm", english),
      recent.toLocaleDateString(english, "ddd"))

    var old = new Date(now.getTime() - 8 * 86400000)
    compare(Model.shortTime(old.getTime() / 1000, "HH:mm", dutch),
      old.toLocaleDateString(dutch, "dd MMM"))
    compare(Model.shortTime(old.getTime() / 1000, "HH:mm", english),
      old.toLocaleDateString(english, "dd MMM"))
  }

  function test_messageTime() {
    compare(Model.messageTime(null, "HH:mm"), "")
    var timestamp = new Date(2024, 0, 2, 13, 7, 0).getTime() / 1000
    compare(Model.messageTime(timestamp, "HH:mm"), "13:07")
    compare(Model.messageTime(timestamp, "h:mm AP"), Qt.formatTime(new Date(timestamp * 1000), "h:mm AP"))
    compare(Model.messageTime(timestamp), Qt.formatTime(new Date(timestamp * 1000), "HH:mm"))
  }

  function test_lastSeenLabel() {
    compare(Model.lastSeenLabel(null, 1, "HH:mm"), "")
    compare(Model.lastSeenLabel("invalid", 1, "HH:mm"), "")
    compare(Model.lastSeenLabel(1, "invalid", "HH:mm"), "")

    var now = new Date(2024, 7, 28, 15, 30, 0)
    var today = new Date(2024, 7, 28, 13, 7, 0)
    var yesterday = new Date(2024, 7, 27, 23, 5, 0)
    var older = new Date(2024, 6, 3, 9, 4, 0)
    var locale = Qt.locale("en_US")
    compare(Model.lastSeenLabel(today.getTime() / 1000,
      now.getTime() / 1000, "HH:mm", locale), "last seen today at 13:07")
    compare(Model.lastSeenLabel(yesterday.getTime() / 1000,
      now.getTime() / 1000, "HH:mm", locale), "last seen yesterday at 23:05")
    compare(Model.lastSeenLabel(older.getTime() / 1000,
      now.getTime() / 1000, "h:mm AP", locale),
      "last seen " + older.toLocaleDateString(locale, "d MMM") + " at 9:04 AM")
  }

  function test_mediaDuration_data() {
    return [
      { tag: "invalid", input: "invalid", expected: "0:00" },
      { tag: "negative", input: -4, expected: "0:00" },
      { tag: "fraction", input: 61.9, expected: "1:01" },
      { tag: "hours", input: 3661, expected: "1:01:01" },
      { tag: "large minutes", input: 3905, expected: "1:05:05" }
    ]
  }

  function test_mediaDuration(data) {
    compare(Model.mediaDuration(data.input), data.expected)
  }

  function test_fileSize_data() {
    return [
      { tag: "missing", input: 0, expected: "Unknown size" },
      { tag: "malformed", input: "invalid", expected: "Unknown size" },
      { tag: "negative", input: -1, expected: "Unknown size" },
      { tag: "bytes", input: 9, expected: "9 B" },
      { tag: "fractional kilobytes", input: 1536, expected: "1.5 KB" },
      { tag: "whole kilobytes", input: 10240, expected: "10 KB" },
      { tag: "megabytes", input: 1048576, expected: "1.0 MB" },
      { tag: "gigabyte cap", input: 1099511627776, expected: "1024 GB" }
    ]
  }

  function test_fileSize(data) {
    compare(Model.fileSize(data.input), data.expected)
  }

  function test_documentDetails_data() {
    return [
      { tag: "pdf singular", mime: "application/pdf", bytes: 1024, pages: 1, expected: "PDF · 1.0 KB · 1 page" },
      { tag: "generic plural", mime: "text/plain", bytes: 20, pages: 2, expected: "PLAIN · 20 B · 2 pages" },
      { tag: "default mime", mime: null, bytes: 0, pages: 0, expected: "OCTET-STREAM · Unknown size" },
      { tag: "empty subtype", mime: "application/", bytes: 1, pages: -1, expected: "Document · 1 B" }
    ]
  }

  function test_documentDetails(data) {
    compare(Model.documentDetails(data.mime, data.bytes, data.pages), data.expected)
  }

  function test_escapeHtml() {
    compare(Model.escapeHtml(null), "")
    compare(Model.escapeHtml("<&>\"'"), "&lt;&amp;&gt;&quot;&#39;")
    compare(Model.escapedMessageText("one\r\ntwo\rthree\nfour"),
      "one<br>two<br>three<br>four")
  }

  function test_characterCount() {
    compare(Model.characterCount("banana", "a"), 3)
    compare(Model.characterCount("banana", "z"), 0)
  }

  function test_trimmedLink_data() {
    return [
      { tag: "plain", input: "https://example.test/path", url: "https://example.test/path", suffix: "" },
      { tag: "punctuation", input: "https://example.test?!", url: "https://example.test", suffix: "?!" },
      { tag: "balanced parentheses", input: "https://example.test/a_(b)", url: "https://example.test/a_(b)", suffix: "" },
      { tag: "unbalanced parentheses", input: "https://example.test/a)", url: "https://example.test/a", suffix: ")" },
      { tag: "unbalanced brackets", input: "https://example.test/a]}", url: "https://example.test/a", suffix: "]}" },
      { tag: "empty", input: null, url: "", suffix: "" }
    ]
  }

  function test_trimmedLink(data) {
    var result = Model.trimmedLink(data.input)
    compare(result.url, data.url)
    compare(result.suffix, data.suffix)
  }

  function test_linkifiedMessage_data() {
    return [
      { tag: "plain escaped", input: "<hello>\nworld", color: "#fff", expected: "&lt;hello&gt;<br>world" },
      { tag: "https", input: "See https://example.test/a.", color: "#12\"34", expected: "See <a href=\"https://example.test/a\"><font color=\"#12&quot;34\">https://example.test/a</font></a>." },
      { tag: "www", input: "www.example.test", color: "red", expected: "<a href=\"https://www.example.test\"><font color=\"red\">www.example.test</font></a>" },
      { tag: "multiple and hostile", input: "<b> https://one.test & www.two.test/x)", color: "green", expected: "&lt;b&gt; <a href=\"https://one.test\"><font color=\"green\">https://one.test</font></a> &amp; <a href=\"https://www.two.test/x\"><font color=\"green\">www.two.test/x</font></a>)" }
    ]
  }

  function test_linkifiedMessage(data) {
    compare(Model.linkifiedMessage(data.input, data.color), data.expected)
  }

  function test_connectionLabel_data() {
    return [
      { tag: "connected", input: "connected", expected: "Connected" },
      { tag: "pairing", input: "pairing", expected: "Link your phone" },
      { tag: "logged out", input: "logged_out", expected: "Unlinked" },
      { tag: "disconnected", input: "disconnected", expected: "Reconnecting" },
      { tag: "error", input: "error", expected: "Connection error" },
      { tag: "starting", input: "starting", expected: "Starting" },
      { tag: "missing", input: null, expected: "Starting" },
      { tag: "unknown", input: "future", expected: "Starting" }
    ]
  }

  function test_connectionLabel(data) {
    compare(Model.connectionLabel(data.input), data.expected)
  }
}
