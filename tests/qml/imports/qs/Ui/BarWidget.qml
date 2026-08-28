import QtQuick

Item {
  property QtObject bar: null
  property string moduleName: ""
  property var settings: ({})
  function setting(name, fallback) {
    var value = settings ? settings[name] : undefined
    return value === undefined || value === null ? fallback : value
  }
}
