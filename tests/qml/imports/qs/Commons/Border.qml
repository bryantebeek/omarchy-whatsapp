pragma Singleton

import QtQuick

QtObject {
  function none() {
    return { color: "transparent", widths: { top: 0, right: 0, bottom: 0, left: 0 } }
  }
  function flat(color, width) {
    var value = Number(width || 0)
    return { color: color, widths: { top: value, right: value, bottom: value, left: value } }
  }
  function controlSpec() { return none() }
  function localOrSurfaceSpec(spec) { return spec || none() }
  function color(spec) { return spec && spec.color ? spec.color : "transparent" }
  function top(spec) { return spec && spec.widths ? Number(spec.widths.top || 0) : 0 }
  function right(spec) { return spec && spec.widths ? Number(spec.widths.right || 0) : 0 }
  function bottom(spec) { return spec && spec.widths ? Number(spec.widths.bottom || 0) : 0 }
  function left(spec) { return spec && spec.widths ? Number(spec.widths.left || 0) : 0 }
}
