import QtQuick
import QtQuick.Controls as QQC
import Quickshell.Io
import qs.Commons
import qs.Ui

QQC.Popup {
  id: root

  property real devicePixelRatio: 1
  property var entries: []
  property string loadError: ""

  readonly property color accent: Color.accent
  readonly property string fontFamily: Style.font.family
  readonly property string reportPath: {
    var value = String(Qt.resolvedUrl("licenses.json"))
    return value.indexOf("file://") === 0
      ? decodeURIComponent(value.substring(7)) : value
  }
  readonly property var popupBorderSpec:
    Border.localOrSurfaceSpec("popups", "border",
      Color.popups.border, Color.popups.border,
      Math.max(1, Style.normalBorderWidth))

  function devicePixelBorderSpec(spec) {
    var scale = Math.max(1, Number(devicePixelRatio) || 1)
    function snappedWidth(value) {
      var width = Math.max(0, Number(value) || 0)
      return width > 0 ? Math.max(1, Math.round(width * scale)) / scale : 0
    }
    var borderColor = Border.color(spec)
    var gradient = spec && spec.gradient && spec.gradient.enabled
      ? spec.gradient
      : { colors: [borderColor, borderColor], angle: 0, enabled: true }
    return {
      color: borderColor,
      widths: {
        top: snappedWidth(Border.top(spec)),
        right: snappedWidth(Border.right(spec)),
        bottom: snappedWidth(Border.bottom(spec)),
        left: snappedWidth(Border.left(spec))
      },
      gradient: gradient
    }
  }

  function filtered(query) {
    var needle = String(query || "").trim().toLowerCase()
    if (!needle) return entries
    var matches = []
    for (var i = 0; i < entries.length; i++) {
      var entry = entries[i] || {}
      var haystack = (String(entry.name || "") + "\n"
        + String(entry.version || "") + "\n"
        + String(entry.license || "")).toLowerCase()
      if (haystack.indexOf(needle) >= 0) matches.push(entry)
    }
    return matches
  }

  function kindLabel(kind) {
    if (kind === "project") return "Application"
    if (kind === "asset") return "Bundled asset"
    return "Rust package"
  }

  component CrispBorderSurface: BorderSurface {
    property var sourceBorderSpec: Border.none()
    borderSpec: root.devicePixelBorderSpec(sourceBorderSpec)
  }

  component CrispButton: Button {
    borderSpec: root.devicePixelBorderSpec(_borderSpec)
  }

  component CrispTextField: TextField {
    id: crispTextField
    background: CrispBorderSurface {
      color: Style.controlFill(crispTextField._focused,
        crispTextField._hot, crispTextField.foreground, crispTextField.accent)
      sourceBorderSpec: crispTextField._borderSpec
      radius: Style.cornerRadius
    }
  }

  FileView {
    path: root.reportPath
    printErrors: false
    watchChanges: true
    onLoaded: {
      try {
        var report = JSON.parse(text())
        root.entries = report && Array.isArray(report.entries) ? report.entries : []
        root.loadError = ""
      } catch (error) {
        root.entries = []
        root.loadError = "The bundled license report could not be read."
      }
    }
    onLoadFailed: {
      root.entries = []
      root.loadError = "The bundled license report is unavailable."
    }
  }

  x: Math.round((parent.width - width) / 2)
  y: Math.round((parent.height - height) / 2)
  width: Math.min(Style.space(760), parent.width - Style.space(48))
  height: Math.min(Style.space(620), parent.height - Style.space(48))
  padding: 0
  leftPadding: Border.left(popupBorderSpec)
  rightPadding: Border.right(popupBorderSpec)
  topPadding: Border.top(popupBorderSpec)
  bottomPadding: Border.bottom(popupBorderSpec)
  modal: true
  focus: true
  closePolicy: QQC.Popup.CloseOnEscape | QQC.Popup.CloseOnPressOutside

  onOpened: {
    licenseSearch.text = ""
    Qt.callLater(function() { licenseSearch.forceActiveFocus() })
  }

  background: CrispBorderSurface {
    color: Color.popups.background
    sourceBorderSpec: root.popupBorderSpec
    radius: Style.cornerRadius + Style.space(4)
  }

  contentItem: Column {
    spacing: 0

    Item {
      id: licensesHeader

      width: parent.width
      height: Style.space(68)

      Column {
        anchors.left: parent.left
        anchors.leftMargin: Style.space(18)
        anchors.verticalCenter: parent.verticalCenter
        spacing: Style.space(2)

        Text {
          text: "Open-source licenses"
          color: Color.popups.text
          font.family: root.fontFamily
          font.pixelSize: Style.font.title
          font.bold: true
        }
        Text {
          text: root.entries.length + " licensed components"
          color: Qt.rgba(Color.popups.text.r, Color.popups.text.g,
            Color.popups.text.b, Color.popups.text.a * 0.72)
          font.family: root.fontFamily
          font.pixelSize: Style.font.caption
        }
      }

      CrispButton {
        anchors.right: parent.right
        anchors.rightMargin: Style.space(10)
        anchors.verticalCenter: parent.verticalCenter
        iconText: "󰅖"
        foreground: Color.popups.text
        accent: root.accent
        tooltipText: "Close licenses"
        focusable: true
        onClicked: root.close()
      }
    }

    Rectangle {
      width: parent.width
      height: Math.max(1, Style.normalBorderWidth)
      color: Color.popups.border
    }

    Item {
      id: licenseSearchRow

      width: parent.width
      height: Style.space(58)

      CrispTextField {
        id: licenseSearch

        anchors.fill: parent
        anchors.margins: Style.space(10)
        placeholderText: "Search packages or licenses"
        foreground: Color.popups.text
        accent: root.accent
      }
    }

    Rectangle {
      width: parent.width
      height: Math.max(1, Style.normalBorderWidth)
      color: Color.popups.border
    }

    Item {
      width: parent.width
      height: parent.height - licensesHeader.height
        - licenseSearchRow.height - Style.normalBorderWidth * 2

      ListView {
        id: licenseList

        anchors.fill: parent
        anchors.margins: Style.space(6)
        clip: true
        spacing: Style.space(2)
        model: root.filtered(licenseSearch.text)

        delegate: Rectangle {
          required property var modelData
          required property int index

          width: licenseList.width
          height: Style.space(56)
          radius: Style.cornerRadius
          color: index % 2 === 0
            ? Style.hoverFillFor(Color.popups.text, root.accent) : "transparent"

          Column {
            anchors.left: parent.left
            anchors.leftMargin: Style.space(10)
            anchors.right: licenseValue.left
            anchors.rightMargin: Style.space(16)
            anchors.verticalCenter: parent.verticalCenter
            spacing: Style.space(2)

            Text {
              width: parent.width
              text: String(modelData.name || "")
                + (modelData.version ? " " + modelData.version : "")
              color: Color.popups.text
              font.family: root.fontFamily
              font.pixelSize: Style.font.body
              font.bold: modelData.kind !== "rust"
              elide: Text.ElideRight
            }
            Text {
              width: parent.width
              text: root.kindLabel(String(modelData.kind || ""))
              color: Qt.rgba(Color.popups.text.r, Color.popups.text.g,
                Color.popups.text.b, Color.popups.text.a * 0.68)
              font.family: root.fontFamily
              font.pixelSize: Style.font.caption
              elide: Text.ElideRight
            }
          }

          Text {
            id: licenseValue

            anchors.right: parent.right
            anchors.rightMargin: Style.space(10)
            anchors.verticalCenter: parent.verticalCenter
            width: Math.min(parent.width * 0.46, implicitWidth)
            text: String(modelData.license || "Not declared")
            color: root.accent
            font.family: root.fontFamily
            font.pixelSize: Style.font.caption
            horizontalAlignment: Text.AlignRight
            elide: Text.ElideRight
          }
        }
      }

      Text {
        anchors.centerIn: parent
        visible: licenseList.count === 0
        text: root.loadError !== "" ? root.loadError : "No matching licenses"
        color: Qt.rgba(Color.popups.text.r, Color.popups.text.g,
          Color.popups.text.b, Color.popups.text.a * 0.72)
        font.family: root.fontFamily
        font.pixelSize: Style.font.body
      }
    }
  }
}
