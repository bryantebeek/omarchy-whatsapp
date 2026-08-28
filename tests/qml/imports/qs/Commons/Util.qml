pragma Singleton

import QtQuick

QtObject {
  function alpha(color, value) { return Qt.rgba(color.r, color.g, color.b, value) }
}
