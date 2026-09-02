import qs.Ui

import "DevicePixel.js" as DevicePixel

// Button whose state border keeps the same crisp widths as the surfaces
// around it.
Button {
  id: root

  property real devicePixelRatio: 1

  borderSpec: DevicePixel.borderSpec(_borderSpec, devicePixelRatio)
}
