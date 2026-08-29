#!/usr/bin/env python3
"""Run deterministic semantic mutants through the real Qt Quick Test suites."""

from __future__ import annotations

import os
import shutil
import subprocess
import sys
import tempfile
from dataclasses import dataclass
from pathlib import Path


REPO = Path(__file__).resolve().parent.parent


@dataclass(frozen=True)
class Mutation:
    name: str
    source: str
    test: str
    old: str
    new: str


def mutation(name: str, source: str, test: str, old: str, new: str) -> Mutation:
    return Mutation(name, source, test, old, new)


M = "quickshell/Model.js"
S = "quickshell/Service.qml"
P = "quickshell/Panel.qml"
TM = "tests/qml/tst_model.qml"
TS = "tests/qml/tst_service.qml"
TP = "tests/qml/tst_workflows.qml"
TC = "tests/qml/tst_components.qml"

MUTATIONS = [
    mutation("model-copy-drops-input", M, TM, "value = value || {}", "value = {}"),
    mutation("model-empty-jid-accepted", M, TM, 'if (!raw) return ""', 'if (raw) return ""'),
    mutation("model-existing-jid-rejected", M, TM, 'if (raw.indexOf("@") >= 0) return raw', 'if (raw.indexOf("@") < 0) return raw'),
    mutation("model-jid-keeps-nondigits", M, TM, 'var digits = raw.replace(/[^0-9]/g, "")', 'var digits = raw.replace(/[0-9]/g, "")'),
    mutation("model-name-fallback-condition", M, TM, "if (value && value !== address && value.indexOf(\"@lid\") < 0) return value", "if (value || value !== address || value.indexOf(\"@lid\") < 0) return value"),
    mutation("model-phone-jid-kind", M, TM, 'if (!value && address.indexOf("@s.whatsapp.net") > 0)', 'if (!value && address.indexOf("@s.whatsapp.net") < 0)'),
    mutation("model-initials-single-word", M, TM, "if (parts.length === 1) return parts[0].substr(0, 2).toUpperCase()", "if (parts.length !== 1) return parts[0].substr(0, 2).toUpperCase()"),
    mutation("model-date-quote-detection", M, TM, "if (input.charAt(i) !== \"'\")", "if (input.charAt(i) === \"'\")"),
    mutation("model-date-escaped-quote", M, TM, "if (i + 1 < input.length && input.charAt(i + 1) === \"'\") {\n      i++", "if (i + 1 < input.length && input.charAt(i + 1) === \"'\") {\n      i += 2"),
    mutation("model-clock-array-detection", M, TM, "var candidates = Array.isArray(clockFormats) ? clockFormats : [clockFormats]", "var candidates = !Array.isArray(clockFormats) ? clockFormats : [clockFormats]"),
    mutation("model-clock-minute-default", M, TM, '(minute ? minute[0] : "mm")', '(minute ? "mm" : minute[0])'),
    mutation("model-clock-12-hour-detection", M, TM, 'if (hour[0].charAt(0) === "h")', 'if (hour[0].charAt(0) !== "h")'),
    mutation("model-short-time-zero", M, TM, 'function shortTime(seconds, format, locale) {\n  var value = Number(seconds || 0)\n  if (!value) return ""', 'function shortTime(seconds, format, locale) {\n  var value = Number(seconds || 0)\n  if (value) return ""'),
    mutation("model-short-time-locale", M, TM,
             "function shortTime(seconds, format, locale) {\n"
             "  var value = Number(seconds || 0)\n"
             "  if (!value) return \"\"\n"
             "  var date = new Date(value * 1000)\n"
             "  var today = new Date()\n"
             "  var activeLocale = locale || Qt.locale()",
             "function shortTime(seconds, format, locale) {\n"
             "  var value = Number(seconds || 0)\n"
             "  if (!value) return \"\"\n"
             "  var date = new Date(value * 1000)\n"
             "  var today = new Date()\n"
             '  var activeLocale = Qt.locale("C")'),
    mutation("model-short-time-today", M, TM, "if (date.toDateString() === today.toDateString())", "if (date.toDateString() !== today.toDateString())"),
    mutation("model-short-time-week-boundary", M, TM, "if (age < 6 * 86400000)", "if (age < 1 * 86400000)"),
    mutation("model-message-default-format", M, TM, 'return Qt.formatTime(new Date(value * 1000), format || "HH:mm")', 'return Qt.formatTime(new Date(value * 1000), format || "ss")'),
    mutation("model-duration-nan-guard", M, TM, "var value = isFinite(parsed) ? Math.max(0, Math.floor(parsed)) : 0", "var value = !isFinite(parsed) ? Math.max(0, Math.floor(parsed)) : 0"),
    mutation("model-duration-hours", M, TM, "if (!hours) return minutes + \":\" + paddedSeconds", "if (hours) return minutes + \":\" + paddedSeconds"),
    mutation("model-file-nan-guard", M, TM, "var value = isFinite(parsed) ? Math.max(0, parsed) : 0", "var value = !isFinite(parsed) ? Math.max(0, parsed) : 0"),
    mutation("model-file-empty", M, TM, 'if (!value) return "Unknown size"', 'if (value) return "Unknown size"'),
    mutation("model-file-unit-boundary", M, TM, "while (value >= 1024 && unit < units.length - 1)", "while (value > 1024 && unit < units.length - 1)"),
    mutation("model-file-precision", M, TM, "var precision = unit === 0 || value >= 10 ? 0 : 1", "var precision = unit !== 0 || value < 10 ? 0 : 1"),
    mutation("model-document-pdf", M, TM, 'var type = mime === "application/pdf" ? "PDF" : mime.split("/").pop().toUpperCase()', 'var type = mime !== "application/pdf" ? "PDF" : mime.split("/").pop().toUpperCase()'),
    mutation("model-document-page-plural", M, TM, 'pages === 1 ? " page" : " pages"', 'pages !== 1 ? " page" : " pages"'),
    mutation("model-html-ampersand", M, TM, '.replace(/&/g, "&amp;")', '.replace(/&/g, "&quot;")'),
    mutation("model-newline-rendering", M, TM, '.replace(/\\r\\n|\\r|\\n/g, "<br>")', '.replace(/\\r\\n/g, "<br>")'),
    mutation("model-balanced-link-punctuation", M, TM, "characterCount(url, last) > characterCount(url, pairs[last])", "characterCount(url, last) >= characterCount(url, pairs[last])"),
    mutation("model-link-www-detection", M, TM, "var pattern = /(?:https?:\\/\\/|www\\.)[^\\s<>\"']+/gi", "var pattern = /(?:https?:\\/\\/)[^\\s<>\"']+/gi"),
    mutation("model-link-www-scheme", M, TM, 'parts.url.indexOf("www.") === 0', 'parts.url.indexOf("www.") !== 0'),
    mutation("model-connected-label", M, TM, 'if (value === "connected") return "Connected"', 'if (value === "connected") return "Starting"'),
    mutation("model-last-seen-today", M, TM, "&& date.getDate() === now.getDate())", "&& date.getDate() !== now.getDate())"),
    mutation("model-last-seen-yesterday", M, TM, "&& date.getDate() === yesterday.getDate())", "&& date.getDate() !== yesterday.getDate())"),

    mutation("service-array-validation", S, TS, "return Array.isArray(value) ? value.slice() : []", "return !Array.isArray(value) ? value.slice() : []"),
    mutation("service-normalize-delivery-time", S, TS, "isFinite(deliveredAt) && deliveredAt > 0", "isFinite(deliveredAt) && deliveredAt < 0"),
    mutation("service-normalize-recipient-delivery-time", S, TS, "isFinite(recipientDeliveredAt) && recipientDeliveredAt > 0", "isFinite(recipientDeliveredAt) && recipientDeliveredAt < 0"),
    mutation("service-normalize-reader-time", S, TS, "isFinite(readerReadAt) && readerReadAt > 0", "isFinite(readerReadAt) && readerReadAt < 0"),
    mutation("service-delivery-receipt-only", S, TS, "if (nextReceipt === 2 && eventTimestamp", "if (nextReceipt >= 2 && eventTimestamp"),
    mutation("service-earliest-recipient-delivery-time", S, TS, "nextDelivery.delivered_at < currentDeliveredAt", "nextDelivery.delivered_at > currentDeliveredAt"),
    mutation("service-earliest-read-time", S, TS, "eventTimestamp < readAt", "eventTimestamp > readAt"),
    mutation("service-earliest-reader-time", S, TS, "nextReader.read_at < currentReaderAt", "nextReader.read_at > currentReaderAt"),
    mutation("service-message-position-preservation", S, TS, "messagesWillChange(preservePosition === true)", "messagesWillChange(preservePosition !== true)"),
    mutation("service-current-conversation-preservation", S, TS, "var preserveMessagePosition = messagesChatJid === selectedChatJid", "var preserveMessagePosition = messagesChatJid !== selectedChatJid"),
    mutation("service-group-pending-match", S, TS, 'String(groupParticipantRequestJids[requestId] || "") === value', 'String(groupParticipantRequestJids[requestId] || "") !== value'),
    mutation("service-preference-value", S, TS, "parsed && parsed.unread_only === true", "parsed && parsed.unread_only === false"),
    mutation("service-unread-filter-value", S, TS, "var next = value === true", "var next = value !== true"),
    mutation("service-avatar-self", S, TS, 'if (!jid || String(jid) === "me") return ""', 'if (!jid || String(jid) !== "me") return ""'),
    mutation("service-file-url-empty", S, TS, 'return path ? "file://" + String(path) + "?v=" + version : ""', 'return !path ? "file://" + String(path) + "?v=" + version : ""'),
    mutation("service-send-disconnected", S, TS, "if (!socket || !socket.connected) return 0", "if (!socket && !socket.connected) return 0"),
    mutation("service-message-queue", S, TS, "if (queueIfPending === true)", "if (queueIfPending !== true)"),
    mutation("service-finish-current-request", S, TS, 'if (String(requestIds[jid] || "") === requestId) delete requestIds[jid]', 'if (String(requestIds[jid] || "") !== requestId) delete requestIds[jid]'),
    mutation("service-media-override-revision", S, TS, "return revision > 0 ? mediaOverrides[key]", "return revision <= 0 ? mediaOverrides[key]"),
    mutation("service-media-selected-chat", S, TS, 'String(frame.chat_jid || "") !== selectedChatJid', 'String(frame.chat_jid || "") === selectedChatJid'),
    mutation("service-image-download-command", S, TS, 'kind === "image" ? "download_image"', 'kind === "image" ? "download_video"'),
    mutation("service-sticker-download-command", S, TS, 'kind === "sticker" ? "download_sticker"', 'kind === "sticker" ? "download_image"'),
    mutation("service-lottie-download-guard", S, TS, 'kind === "sticker" && message.media.lottie === true', 'kind === "sticker" && message.media.lottie !== true'),
    mutation("service-auto-sticker-kind", S, TS, 'media.kind !== "sticker" || media.downloaded === true', 'media.kind === "sticker" || media.downloaded === true'),
    mutation("service-auto-sticker-trigger", S, TS, "autoDownloadStickers(messages)", "autoDownloadStickers([])"),
    mutation("service-selection-change", S, TS, "var changed = value !== selectedChatJid", "var changed = value === selectedChatJid"),
    mutation("service-empty-message", S, TS, "if (!selectedChatJid || !body.trim()) return false", "if (!selectedChatJid && !body.trim()) return false"),
    mutation("service-chat-pin-value", S, TS, "pinned: pinned === true", "pinned: pinned !== true"),
    mutation("service-unlink-state", S, TS, 'if (connectionState !== "connected") return false', 'if (connectionState === "connected") return false'),
    mutation("service-reaction-owner", S, TS, "target_from_me: message.from_me === true", "target_from_me: message.from_me !== true"),
    mutation("service-active-chat-focus", S, TS, "panelVisible && panelFocused && selectedChatJid", "panelVisible || panelFocused || selectedChatJid"),
    mutation("service-state-event", S, TS, 'frame.event === "state"', 'frame.event === "mutated_state"'),
    mutation("service-stale-messages", S, TS, '} else if (frame.event === "messages") {\n      if (String(frame.chat_jid || "") === selectedChatJid)', '} else if (frame.event === "messages") {\n      if (String(frame.chat_jid || "") !== selectedChatJid)'),
    mutation("service-unread-field", S, TS, "unreadTotal = Number(frame.total || 0)", "unreadTotal = Number(frame.unread_total || 0)"),
    mutation("service-error-default", S, TS, 'String(frame.message || "WhatsApp command failed")', 'String(frame.message || "Mutated error")'),
    mutation("service-presence-available", S, TS, "available: frame.available === true", "available: frame.available !== true"),
    mutation("service-chat-state-pause", S, TS, 'if (state === "paused") {', 'if (state !== "paused") {'),
    mutation("service-chat-state-expiry", S, TS, "Number((values[key] || {}).expires_at || 0) > now", "Number((values[key] || {}).expires_at || 0) <= now"),
    mutation("service-direct-chat-state-label", S, TS, 'if (isGroup !== true)\n      return recordingNames.length ? "recording audio…" : "typing…"', 'if (isGroup === true)\n      return recordingNames.length ? "recording audio…" : "typing…"'),
    mutation("service-mixed-group-chat-state", S, TS, "if (recordingNames.length && typingNames.length)", "if (recordingNames.length || typingNames.length)"),
    mutation("service-local-typing-connected", S, TS, '&& selectedChatJid && connectionState === "connected"', '&& selectedChatJid && connectionState !== "connected"'),

    mutation("panel-paired-state", P, TP, 'service.connectionState === "connected"', 'service.connectionState !== "connected"'),
    mutation("panel-chat-source-validation", P, TP, "service && Array.isArray(service.chats) ? service.chats : []", "service && !Array.isArray(service.chats) ? service.chats : []"),
    mutation("panel-unread-filter", P, TP, "Number(chat.unread || 0) <= 0 && !isSelected", "Number(chat.unread || 0) > 0 && !isSelected"),
    mutation("panel-chat-last-message-search", P, TP, '+ String(chat.last_message || "") + "\\n" + String(chat.jid || "")', '+ "" + "\\n" + String(chat.jid || "")'),
    mutation("panel-open-target", P, TP, "if (targetJid) service.selectChat(targetJid)", "if (!targetJid) service.selectChat(targetJid)"),
    mutation("panel-new-chat-normalization", P, TP, "var jid = Model.normalizedJid(newChat.text)", "var jid = String(newChat.text || \"\")"),
    mutation("panel-submit-success", P, TP, "if (!service || !service.sendMessage(composer.text)) return", "if (!service || service.sendMessage(composer.text)) return"),
    mutation("panel-app-menu-auto-focus", P, TP, 'objectName: "headerMenu"\n                readonly property var popupBorderSpec:', 'objectName: "headerMenu"\n                onOpened: Qt.callLater(function() { headerLicenseAction.forceActiveFocus() })\n                readonly property var popupBorderSpec:'),
    mutation("panel-chat-menu-auto-focus", P, TP, 'objectName: "chatContextMenu-"\n                          + String(modelData.jid || "")\n                        readonly property var popupBorderSpec:', 'objectName: "chatContextMenu-"\n                          + String(modelData.jid || "")\n                        onOpened: Qt.callLater(function() { chatPinAction.forceActiveFocus() })\n                        readonly property var popupBorderSpec:'),
    mutation("panel-chat-pin-toggle", P, TP, "modelData.pinned !== true)", "modelData.pinned === true)"),
    mutation("panel-image-preview-url", P, TP, "imagePreviewUrl = service.fileUrl(path, revision)", "imagePreviewUrl = String(path || \"\")"),
    mutation("panel-sticker-kind", P, TP, 'readonly property bool isSticker: mediaData\n                        && mediaData.kind === "sticker"', 'readonly property bool isSticker: mediaData\n                        && mediaData.kind === "image"'),
    mutation("panel-sticker-preview-selection", P, TP, 'readonly property string displayPath: downloaded\n                              ? mediaPath : thumbnailPath', 'readonly property string displayPath: downloaded\n                              ? thumbnailPath : mediaPath'),
    mutation("panel-sticker-download-status", P, TP, 'readonly property bool active: stickerCard.active\n                                && !stickerCard.lottie && !stickerCard.downloaded', 'readonly property bool active: stickerCard.active\n                                && stickerCard.lottie && !stickerCard.downloaded'),
    mutation("panel-group-error", P, TC, 'if (service.groupParticipantsError) return "Participants unavailable"', 'if (!service.groupParticipantsError) return "Participants unavailable"'),
    mutation("panel-live-minutes-rounding", P, TC, "Math.ceil(\n      (Number(untilTimestamp || 0) - currentTimestamp) / 60)", "Math.floor(\n      (Number(untilTimestamp || 0) - currentTimestamp) / 60)"),
    mutation("panel-activity-precedence", P, TC, "if (activity) return activity", "if (!activity) return activity"),
    mutation("panel-sidebar-direct-activity", P, TC, "if (!activity || chat.is_group === true) return String(activity || \"\")", "if (!activity || chat.is_group !== true) return String(activity || \"\")"),
    mutation("panel-sidebar-activity-preview", P, TP, "if (chatDelegate.activityText)\n                                  return chatDelegate.activityText", "if (!chatDelegate.activityText)\n                                  return chatDelegate.activityText"),
    mutation("panel-sidebar-activity-color", P, TP, "color: chatDelegate.activityText\n                                ? root.accent : root.sidebarSecondary", "color: chatDelegate.activityText\n                                ? root.sidebarSecondary : root.accent"),
    mutation("panel-receipt-tooltip-status", P, TC, "label: root.messageReceiptLabel(value.receipt)", 'label: ""'),
    mutation("panel-receipt-tooltip-delivery", P, TC, "if (deliveredEntries.length) groups.push({", "if (!deliveredEntries.length) groups.push({"),
    mutation("panel-receipt-tooltip-read-precedence", P, TC, "readJids[deliveryJid] === true", "readJids[deliveryJid] !== true"),
    mutation("panel-receipt-tooltip-group-spacing", P, TP, "readonly property real groupSpacing: Style.space(6)", "readonly property real groupSpacing: Style.space(0)"),
    mutation("panel-receipt-tooltip-header-alpha", P, TP, "messageReceiptTooltipPopup.tooltipForeground.a\n                                  * 0.72", "messageReceiptTooltipPopup.tooltipForeground.a\n                                  * 1.0"),
    mutation("panel-receipt-icon-size", P, TP, "font.pixelSize: 14\n                            font.letterSpacing:", "font.pixelSize: 13\n                            font.letterSpacing:"),
    mutation("panel-receipt-double-tick-overlap", P, TP, "? -3 : 0", "? 0 : 0"),
    mutation("panel-receipt-timestamp-gap", P, TP, "id: messageFooter\n                        visible: messageDelegate.showMessageTime\n                        anchors.top: reactionsBar.visible\n                          ? reactionsBar.bottom : bubble.bottom\n                        anchors.topMargin: Style.space(3)\n                        x: modelData.from_me\n                          ? bubble.x + bubble.width - width - Style.space(4)\n                          : bubble.x + Style.space(4)\n                        spacing: 8", "id: messageFooter\n                        visible: messageDelegate.showMessageTime\n                        anchors.top: reactionsBar.visible\n                          ? reactionsBar.bottom : bubble.bottom\n                        anchors.topMargin: Style.space(3)\n                        x: modelData.from_me\n                          ? bubble.x + bubble.width - width - Style.space(4)\n                          : bubble.x + Style.space(4)\n                        spacing: 4"),
    mutation("panel-receipt-hover-width", P, TP, "width: messageReceiptStatus.implicitWidth", "width: 0"),
    mutation("panel-receipt-hover-enabled", P, TP, 'objectName: "messageReceiptHoverArea-"\n                              + String(modelData.id || "")\n                            anchors.fill: parent\n                            anchors.margins: -3\n                            acceptedButtons: Qt.NoButton\n                            hoverEnabled: true', 'objectName: "messageReceiptHoverArea-"\n                              + String(modelData.id || "")\n                            anchors.fill: parent\n                            anchors.margins: -3\n                            acceptedButtons: Qt.NoButton\n                            hoverEnabled: false'),
    mutation("panel-message-anchor-offset", P, TP, "preservedConversationMessageOffset = item\n        ? item.y - messageList.contentY : 0", "preservedConversationMessageOffset = item\n        ? item.y + messageList.contentY : 0"),
    mutation("panel-incremental-message-model", P, TP, "model: conversationMessageModel", "model: root.service ? root.service.messages : []"),
    mutation("panel-message-row-change", P, TP, "if (row.messageJson !== serialized)", "if (row.messageJson === serialized)"),
    mutation("panel-incremental-chat-model", P, TP, "model: chatRenderModel", "model: root.filteredChats"),
    mutation("panel-chat-row-change", P, TP, "if (row.chatJson !== serialized)", "if (row.chatJson === serialized)"),
    mutation("panel-conversation-activity-color", P, TP, "color: showingChatActivity\n                            ? root.accent : root.foreground", "color: showingChatActivity\n                            ? root.foreground : root.accent"),
]


def runner() -> str:
    found = shutil.which("qmltestrunner")
    if found:
        return found
    fallback = Path("/usr/lib/qt6/bin/qmltestrunner")
    if fallback.is_file():
        return str(fallback)
    raise RuntimeError("qmltestrunner is required")


def run_test(root: Path, test: str) -> subprocess.CompletedProcess[str]:
    env = os.environ.copy()
    env["QT_QPA_PLATFORM"] = env.get("QML_TEST_PLATFORM", "offscreen")
    env.setdefault("QT_QUICK_BACKEND", "software")
    env.setdefault("QSG_RHI_BACKEND", "software")
    env["QML_DISABLE_DISK_CACHE"] = "1"
    return subprocess.run(
        [
            runner(), "-input", str(root / test),
            "-import", str(root / "tests/qml/imports"),
            "-import", str(root / "quickshell"),
            "-silent",
        ],
        cwd=root,
        env=env,
        text=True,
        errors="replace",
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        check=False,
    )


def main() -> int:
    invalid = []
    for item in MUTATIONS:
        source = (REPO / item.source).read_text(encoding="utf-8")
        count = source.count(item.old)
        if count != 1:
            invalid.append(f"{item.name}: mutation target occurs {count} times")
    if invalid:
        print("\n".join(invalid), file=sys.stderr)
        return 1

    tests = sorted({item.test for item in MUTATIONS})
    with tempfile.TemporaryDirectory(prefix="omarchy-whatsapp-mutation-") as temp:
        root = Path(temp)
        shutil.copytree(REPO / "quickshell", root / "quickshell")
        shutil.copytree(REPO / "tests/qml", root / "tests/qml")

        for test in tests:
            baseline = run_test(root, test)
            if baseline.returncode != 0:
                print(f"Mutation baseline failed for {test}:\n{baseline.stdout}", file=sys.stderr)
                return 1

        survived = []
        for index, item in enumerate(MUTATIONS, start=1):
            path = root / item.source
            original = path.read_text(encoding="utf-8")
            path.write_text(original.replace(item.old, item.new, 1), encoding="utf-8")
            result = run_test(root, item.test)
            path.write_text(original, encoding="utf-8")
            if result.returncode == 0:
                survived.append(item.name)
                status = "SURVIVED"
            else:
                status = "killed"
            print(f"[{index:02d}/{len(MUTATIONS):02d}] {status}: {item.name}")

    killed = len(MUTATIONS) - len(survived)
    score = killed * 100.0 / len(MUTATIONS)
    print(f"QML mutation score: {killed}/{len(MUTATIONS)} ({score:.0f}%)")
    if survived:
        print(f"Surviving mutants: {', '.join(survived)}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
