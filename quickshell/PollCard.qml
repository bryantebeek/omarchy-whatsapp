import QtQuick
import qs.Commons

import "DevicePixel.js" as DevicePixel

// Aggregated poll results with the local user's own selection. Only the
// daemon can decrypt votes, so the card renders the counts it is given and
// sends the full selection back on every change.
Item {
  id: root

  property var panel: null
  property var service: null
  property var message: null
  property var media: null
  property bool active: false
  property string messageId: ""
  property real bubbleRadius: 0
  property real devicePixelRatio: 1
  property string fontFamily: Style.font.family
  property real metaFontSize: Style.font.caption
  property color foreground: Color.foreground
  property color accent: Color.accent
  property color secondary: Color.foreground
  readonly property bool ended: Number(media ? media.end_timestamp || 0 : 0) > 0
    && Number(media.end_timestamp) <= (panel ? panel.currentTimestamp : 0)
  readonly property int totalVoters: Number(media ? media.total_voters || 0 : 0)
  readonly property var options: media && media.options
    && typeof media.options.length === "number" ? media.options : []

  function selectedPollOptions() {
    var selected = []
    if (!active) return selected
    for (var i = 0; i < options.length; i++)
      if (options[i].selected_by_me === true)
        selected.push(String(options[i].name || ""))
    return selected
  }

  // WhatsApp replaces a participant's whole selection, so a single-answer poll
  // clears the previous option instead of adding to it.
  function togglePollOption(optionIndex) {
    if (!service || !active || ended || service.pollVotePending(message)) return
    var option = options[optionIndex] || {}
    var name = String(option.name || "")
    if (!name) return
    var selected = selectedPollOptions()
    var selectedIndex = selected.indexOf(name)
    if (Number(media.selectable_count || 1) === 1) {
      selected = selectedIndex >= 0 ? [] : [name]
    } else if (selectedIndex >= 0) {
      selected.splice(selectedIndex, 1)
    } else {
      selected.push(name)
    }
    service.votePoll(message, selected)
  }

  visible: active
  height: visible ? pollContent.implicitHeight : 0

  Column {
    id: pollContent
    width: parent.width
    spacing: Style.space(7)

    Text {
      objectName: "pollQuestionLabel"
      width: parent.width
      text: root.media ? String(root.media.question || "Poll") : "Poll"
      color: root.foreground
      font.family: root.fontFamily
      font.pixelSize: Style.font.body
      font.bold: true
      wrapMode: Text.WrapAtWordBoundaryOrAnywhere
      textFormat: Text.PlainText
    }

    Text {
      objectName: "pollSelectionHint"
      width: parent.width
      text: Number(root.media ? root.media.selectable_count || 1 : 1) > 1
        ? "Select one or more" : "Select one"
      color: root.secondary
      font.family: root.fontFamily
      font.pixelSize: root.metaFontSize
    }

    Repeater {
      model: root.options

      delegate: Item {
        id: pollOption
        objectName: "pollOption-" + String(index)
        required property var modelData
        required property int index
        readonly property bool selected: modelData.selected_by_me === true
        readonly property int votes: Number(modelData.votes || 0)
        readonly property var voterJids: modelData.voter_jids
          && typeof modelData.voter_jids.length === "number"
          ? modelData.voter_jids : []
        width: pollContent.width
        height: Math.max(Style.space(52),
          pollOptionLabel.implicitHeight + Style.space(26))

        DevicePixelButton {
          id: pollOptionButton
          objectName: "pollOptionButton-" + root.messageId
            + "-" + String(pollOption.index)
          anchors.fill: parent
          readonly property color subtleBorderColor: Qt.rgba(root.foreground.r,
            root.foreground.g, root.foreground.b, 0.10)
          radius: root.bubbleRadius
          foreground: root.foreground
          accent: root.accent
          background: Style.normalFillFor(root.foreground, root.accent)
          bordered: true
          borderSpec: DevicePixel.borderSpec(pollOptionButton._showFocusRing
            ? pollOptionButton._focusBorderSpec
            : Border.flat(subtleBorderColor,
              Math.max(1, Style.normalBorderWidth)), root.devicePixelRatio)
          selected: pollOption.selected
          enabled: !root.ended && root.service
            && !root.service.pollVotePending(root.message)
          focusable: true
          onClicked: root.togglePollOption(pollOption.index)
        }

        Rectangle {
          id: pollProgressTrack
          objectName: "pollProgressTrack-" + root.messageId
            + "-" + String(pollOption.index)
          anchors.left: parent.left
          anchors.right: parent.right
          anchors.bottom: parent.bottom
          height: Style.space(9)
          radius: height / 2
          color: Style.normalFillFor(root.foreground, root.accent)

          Rectangle {
            objectName: "pollProgressFill-" + root.messageId
              + "-" + String(pollOption.index)
            anchors.left: parent.left
            anchors.top: parent.top
            anchors.bottom: parent.bottom
            width: root.totalVoters > 0
              ? parent.width * Math.min(1, pollOption.votes / root.totalVoters)
              : 0
            radius: height / 2
            color: root.accent
            opacity: 0.9
          }
        }

        Text {
          id: pollOptionLabel
          anchors.left: parent.left
          anchors.right: pollVoteSummary.left
          anchors.leftMargin: Style.space(10)
          anchors.rightMargin: Style.space(8)
          anchors.verticalCenter: parent.verticalCenter
          anchors.verticalCenterOffset: -Style.space(5)
          text: (pollOption.selected ? "✓  " : "")
            + String(pollOption.modelData.name || "")
          color: root.foreground
          font.family: root.fontFamily
          font.pixelSize: Style.font.body
          wrapMode: Text.WrapAtWordBoundaryOrAnywhere
          textFormat: Text.PlainText
        }

        Item {
          id: pollVoteSummary
          objectName: "pollVoteSummary-" + root.messageId
            + "-" + String(pollOption.index)
          readonly property int avatarSize: Style.space(22)
          readonly property int avatarStride: Style.space(14)
          readonly property real avatarStackWidth: pollOption.voterJids.length > 0
            ? avatarSize + (pollOption.voterJids.length - 1) * avatarStride : 0
          anchors.right: parent.right
          anchors.rightMargin: Style.space(10)
          anchors.verticalCenter: parent.verticalCenter
          anchors.verticalCenterOffset: -Style.space(5)
          width: pollOptionCount.implicitWidth
            + (avatarStackWidth > 0 ? Style.space(5) + avatarStackWidth : 0)
          height: avatarSize

          Text {
            id: pollOptionCount
            objectName: "pollOptionCount-" + root.messageId
              + "-" + String(pollOption.index)
            anchors.left: parent.left
            anchors.verticalCenter: parent.verticalCenter
            text: String(pollOption.votes)
            color: root.secondary
            font.family: root.fontFamily
            font.pixelSize: root.metaFontSize
          }

          Item {
            id: pollVoterStack
            objectName: "pollVoterStack-" + root.messageId
              + "-" + String(pollOption.index)
            anchors.left: pollOptionCount.right
            anchors.leftMargin: Style.space(5)
            anchors.verticalCenter: parent.verticalCenter
            width: pollVoteSummary.avatarStackWidth
            height: pollVoteSummary.avatarSize

            Repeater {
              model: pollOption.voterJids

              delegate: Avatar {
                required property var modelData
                required property int index
                objectName: "pollVoterAvatar-" + root.messageId
                  + "-" + String(pollOption.index) + "-" + String(index)
                imageObjectName: "pollVoterImage-" + root.messageId
                  + "-" + String(pollOption.index) + "-" + String(index)
                x: index * pollVoteSummary.avatarStride
                z: pollOption.voterJids.length - index
                service: root.service
                jid: String(modelData || "")
                name: root.panel
                  ? root.panel.pollVoterName(String(modelData || "")) : ""
                size: pollVoteSummary.avatarSize
                initialsPixelSize: Math.max(7, root.metaFontSize - 2)
                fontFamily: root.fontFamily
                foreground: root.foreground
                accent: root.accent
                devicePixelRatio: root.devicePixelRatio
                requestOnLoad: true
              }
            }
          }
        }
      }
    }

    Text {
      objectName: "pollVoteTotal"
      width: parent.width
      text: root.ended ? "Poll ended"
        : (root.totalVoters === 1 ? "1 vote" : root.totalVoters + " votes")
      color: root.ended ? Color.urgent : root.secondary
      font.family: root.fontFamily
      font.pixelSize: root.metaFontSize
    }
  }
}
