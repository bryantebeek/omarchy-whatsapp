import QtQuick
import QtQuick.Controls as QQC
import qs.Commons
import qs.Ui

// Delivery detail for an outgoing message. The plain tooltip text stays the
// accessible description while the content item renders one labelled section
// per receipt state.
QQC.ToolTip {
  id: root

  property var groups: []
  property string fontFamily: Style.font.family
  readonly property color tooltipBackground: Color.tooltip.background
  readonly property color tooltipForeground: Color.tooltip.text
  readonly property color tooltipBorder: Color.tooltip.border
  readonly property var tooltipBorderSpec:
    Border.localOrSurfaceSpec("tooltip", "border",
      tooltipBorder, Color.tooltip.border,
      Math.max(1, Style.normalBorderWidth))

  delay: 400
  timeout: -1
  padding: 0

  background: BorderSurface {
    color: root.tooltipBackground
    borderSpec: root.tooltipBorderSpec
    radius: 0
  }

  contentItem: Item {
    id: tooltipBody
    readonly property var groups: root.groups
    readonly property color headerColor: Qt.rgba(
      root.tooltipForeground.r, root.tooltipForeground.g,
      root.tooltipForeground.b, root.tooltipForeground.a * 0.72)
    readonly property color detailColor: root.tooltipForeground
    readonly property real groupSpacing: Style.space(6)
    readonly property string contentFontFamily: root.fontFamily
    readonly property real contentFontSize: Style.font.bodySmall
    readonly property real leftInset: Border.left(root.tooltipBorderSpec)
      + Style.spacing.controlPaddingX
    readonly property real rightInset: Border.right(root.tooltipBorderSpec)
      + Style.spacing.controlPaddingX
    readonly property real topInset: Border.top(root.tooltipBorderSpec)
      + Style.spacing.controlPaddingY
    readonly property real bottomInset: Border.bottom(root.tooltipBorderSpec)
      + Style.spacing.controlPaddingY

    implicitWidth: tooltipContent.implicitWidth + leftInset + rightInset
    implicitHeight: tooltipContent.implicitHeight + topInset + bottomInset

    Column {
      id: tooltipContent
      x: parent.leftInset
      y: parent.topInset
      spacing: tooltipBody.groupSpacing

      Repeater {
        model: tooltipBody.groups

        delegate: Column {
          required property var modelData
          spacing: 0

          Text {
            text: parent.modelData.label
            textFormat: Text.PlainText
            color: tooltipBody.headerColor
            font.family: tooltipBody.contentFontFamily
            font.pixelSize: tooltipBody.contentFontSize
          }

          Repeater {
            model: parent.modelData.entries

            delegate: Text {
              required property var modelData
              text: String(modelData || "")
              textFormat: Text.PlainText
              color: tooltipBody.detailColor
              font.family: tooltipBody.contentFontFamily
              font.pixelSize: tooltipBody.contentFontSize
            }
          }
        }
      }
    }
  }
}
