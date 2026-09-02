.pragma library
.import qs.Commons as Commons

// Omarchy border widths are theme values in logical pixels. On fractional-scale
// outputs an unsnapped hairline is painted across two physical rows and looks
// blurred, so every width is rounded to a whole device pixel before it reaches
// a surface. Zero stays zero: a border that is off must not become a hairline.
function borderSpec(spec, devicePixelRatio) {
  var scale = Math.max(1, Number(devicePixelRatio) || 1)
  function snappedWidth(value) {
    var width = Math.max(0, Number(value) || 0)
    return width > 0 ? Math.max(1, Math.round(width * scale)) / scale : 0
  }
  var borderColor = Commons.Border.color(spec)
  var gradient = spec && spec.gradient && spec.gradient.enabled
    ? spec.gradient
    : { colors: [borderColor, borderColor], angle: 0, enabled: true }
  return {
    color: borderColor,
    widths: {
      top: snappedWidth(Commons.Border.top(spec)),
      right: snappedWidth(Commons.Border.right(spec)),
      bottom: snappedWidth(Commons.Border.bottom(spec)),
      left: snappedWidth(Commons.Border.left(spec))
    },
    gradient: gradient
  }
}
