#!/usr/bin/env python3
"""Exercise a small set of high-value semantic faults through Qt Quick tests.

This is intentionally not a coverage metric. Mutants represent regressions in
input validation, request ordering, connection safety, or core user workflows;
visual styling and exhaustive source-text permutations belong in review/tests.
"""

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


M = "quickshell/Model.js"
S = "quickshell/Service.qml"
P = "quickshell/Panel.qml"
TM = "tests/qml/tst_model.qml"
TS = "tests/qml/tst_service.qml"
TP = "tests/qml/tst_workflows.qml"


MUTATIONS = [
    Mutation("jid-empty-input", M, TM, 'if (!raw) return ""', 'if (raw) return ""'),
    Mutation(
        "jid-digit-normalization",
        M,
        TM,
        'var digits = raw.replace(/[^0-9]/g, "")',
        'var digits = raw.replace(/[0-9]/g, "")',
    ),
    Mutation(
        "clock-12-hour-detection",
        M,
        TM,
        'if (hour[0].charAt(0) === "h")',
        'if (hour[0].charAt(0) !== "h")',
    ),
    Mutation(
        "short-time-today",
        M,
        TM,
        "if (date.toDateString() === today.toDateString())",
        "if (date.toDateString() !== today.toDateString())",
    ),
    Mutation(
        "duration-invalid-number",
        M,
        TM,
        "var value = isFinite(parsed) ? Math.max(0, Math.floor(parsed)) : 0",
        "var value = !isFinite(parsed) ? Math.max(0, Math.floor(parsed)) : 0",
    ),
    Mutation(
        "html-ampersand-escaping",
        M,
        TM,
        '.replace(/&/g, "&amp;")',
        '.replace(/&/g, "&quot;")',
    ),
    Mutation(
        "www-link-detection",
        M,
        TM,
        'var pattern = /(?:https?:\\/\\/|www\\.)[^\\s<>"\']+|@[0-9]+/gi',
        'var pattern = /(?:https?:\\/\\/)[^\\s<>"\']+|@[0-9]+/gi',
    ),
    Mutation(
        "mention-token-boundary",
        M,
        TM,
        "if (/[A-Za-z0-9_]/.test(before) || /[A-Za-z0-9_]/.test(after)) continue",
        "if (/[A-Za-z0-9_]/.test(before) && /[A-Za-z0-9_]/.test(after)) continue",
    ),
    Mutation(
        "disconnected-send-gate",
        S,
        TS,
        "if (!socket || !socket.connected || !protocolCompatible) return 0",
        "if (socket || !socket.connected || !protocolCompatible) return 0",
    ),
    Mutation(
        "queued-message-refresh",
        S,
        TS,
        "if (queueIfPending === true)",
        "if (queueIfPending !== true)",
    ),
    Mutation(
        "downloadable-media-validation",
        S,
        TS,
        'if (["image", "sticker", "video", "audio"].indexOf(kind) < 0) return false',
        'if (["image", "sticker", "video", "audio"].indexOf(kind) >= 0) return false',
    ),
    Mutation(
        "text-acceptance-identity",
        S,
        TS,
        'String(frame.delivery_id || "") === String(pending.delivery_id || "")',
        'String(frame.delivery_id || "") !== String(pending.delivery_id || "")',
    ),
    Mutation(
        "stale-generation-rejection",
        S,
        TS,
        "generation < daemonGeneration",
        "generation > daemonGeneration",
    ),
    Mutation(
        "stale-sequence-rejection",
        S,
        TS,
        "sequence <= Number(resourceSequences[key] || 0)",
        "sequence > Number(resourceSequences[key] || 0)",
    ),
    Mutation(
        "focused-read-guard",
        S,
        TS,
        'if (panelVisible && panelFocused)\n      send("mark_read"',
        'if (panelVisible || panelFocused)\n      send("mark_read"',
    ),
    Mutation(
        "message-invalidation-target",
        S,
        TS,
        "key === selectedChatJid && requestMessages(key, true)",
        "key !== selectedChatJid && requestMessages(key, true)",
    ),
    Mutation(
        "active-chat-focus",
        S,
        TS,
        "panelVisible && panelFocused && selectedChatJid",
        "panelVisible || panelFocused || selectedChatJid",
    ),
    Mutation(
        "protocol-handshake",
        S,
        TS,
        "received !== protocolVersion",
        "received === protocolVersion",
    ),
    Mutation(
        "voice-recording-id-validation",
        S,
        TS,
        "value.length > 0 && value.length <= 80",
        "value.length > 80 && value.length <= 80",
    ),
    Mutation(
        "unread-chat-filter",
        P,
        TP,
        "Number(chat.unread || 0) <= 0 && !isSelected",
        "Number(chat.unread || 0) > 0 && !isSelected",
    ),
    Mutation(
        "new-chat-normalization",
        P,
        TP,
        "var jid = Model.normalizedJid(newChat.text)",
        'var jid = String(newChat.text || "")',
    ),
    Mutation(
        "composer-submit",
        P,
        TP,
        "if (!service || !service.sendMessage(composer.text)) return",
        'if (!service || !service.sendMessage("")) return',
    ),
    Mutation(
        "accepted-draft-chat",
        P,
        TP,
        'root.service.selectedChatJid === String(chatJid || "")',
        'root.service.selectedChatJid !== String(chatJid || "")',
    ),
    Mutation(
        "voice-stop-action",
        P,
        TP,
        'voiceRecordingStopAction = sendRecording === true ? "send" : "discard"',
        'voiceRecordingStopAction = sendRecording !== true ? "send" : "discard"',
    ),
    Mutation(
        "chat-pin-toggle",
        P,
        TP,
        "modelData.pinned !== true)",
        "modelData.pinned === true)",
    ),
    Mutation(
        "conversation-anchor-offset",
        P,
        TP,
        "preservedConversationMessageOffset = item\n        ? item.y - messageList.contentY : 0",
        "preservedConversationMessageOffset = item\n        ? item.y + messageList.contentY : 0",
    ),
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
            runner(),
            "-input",
            str(root / test),
            "-import",
            str(root / "tests/qml/imports"),
            "-import",
            str(root / "quickshell"),
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
        count = (REPO / item.source).read_text(encoding="utf-8").count(item.old)
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
                print(
                    f"Mutation baseline failed for {test}:\n{baseline.stdout}",
                    file=sys.stderr,
                )
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
    print(f"Targeted QML mutants killed: {killed}/{len(MUTATIONS)}")
    if survived:
        print(f"Surviving mutants: {', '.join(survived)}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
