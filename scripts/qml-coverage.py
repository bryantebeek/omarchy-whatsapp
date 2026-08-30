#!/usr/bin/env python3
"""Enforce the portable QML behavioral coverage contract.

Qt's open-source Quick Test runner does not report interpreted QML/JavaScript
line coverage. This gate therefore measures the stable contracts we can verify
portably: every Model.js helper, every Service.qml event, every production
entry-point component, and every required UI workflow.
"""

from __future__ import annotations

import re
import sys
from pathlib import Path


REPO = Path(__file__).resolve().parent.parent


def names(pattern: str, text: str) -> set[str]:
    return set(re.findall(pattern, text, flags=re.MULTILINE))


def report(label: str, required: set[str], covered: set[str]) -> bool:
    missing = sorted(required - covered)
    count = len(required) - len(missing)
    percentage = 100.0 if not required else count * 100.0 / len(required)
    print(f"{label}: {count}/{len(required)} ({percentage:.0f}%)")
    if missing:
        print(f"  missing: {', '.join(missing)}", file=sys.stderr)
    return not missing


def main() -> int:
    model = (REPO / "quickshell/Model.js").read_text(encoding="utf-8")
    model_tests = (REPO / "tests/qml/tst_model.qml").read_text(encoding="utf-8")
    service = (REPO / "quickshell/Service.qml").read_text(encoding="utf-8")
    service_tests = (REPO / "tests/qml/tst_service.qml").read_text(encoding="utf-8")
    component_tests = (REPO / "tests/qml/tst_components.qml").read_text(encoding="utf-8")
    workflow_tests = (REPO / "tests/qml/tst_workflows.qml").read_text(encoding="utf-8")

    model_helpers = names(r"^function\s+([A-Za-z_$][\w$]*)\s*\(", model)
    tested_helpers = names(r"\bModel\.([A-Za-z_$][\w$]*)\s*\(", model_tests)

    service_events = names(r'frame\.event\s*===\s*"([a-z_]+)"', service)
    tested_events = {
        event for event in service_events
        if re.search(rf'["\']{re.escape(event)}["\']', service_tests)
    }

    entry_points = {"Panel", "BarWidget", "Service"}
    tested_components = names(r"\bWhatsapp\.(Panel|BarWidget|Service)\s*\{", component_tests + service_tests)

    required_workflows = {
        "open_search_select_and_close",
        "new_conversation_and_send_message",
        "record_and_send_voice_message",
        "presence_and_typing",
        "menus_open_without_auto_focus",
        "resync_chat_state_recovery",
        "pin_and_unpin_conversation",
        "load_history_and_receive_updates",
        "conversation_date_dividers",
        "event_updates_preserve_conversation_viewport",
        "open_media_and_recover_connection",
        "render_and_download_stickers",
    }
    tested_workflows = names(r"function\s+test_([A-Za-z0-9_]+)\s*\(", workflow_tests)

    checks = [
        report("Model.js public-helper coverage", model_helpers, tested_helpers),
        report("Service.qml event coverage", service_events, tested_events),
        report("Production entry-point coverage", entry_points, tested_components),
        report("Required UI workflow coverage", required_workflows, tested_workflows),
    ]
    if not all(checks):
        return 1
    print("Portable QML behavioral contract coverage: 100%")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
