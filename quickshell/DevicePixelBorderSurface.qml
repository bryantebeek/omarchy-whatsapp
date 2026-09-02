import qs.Commons
import qs.Ui

import "DevicePixel.js" as DevicePixel

// Shared by the panel and its popups so both snap borders the same way.
BorderSurface {
  id: root

  property real devicePixelRatio: 1
  property var sourceBorderSpec: Border.none()

  borderSpec: DevicePixel.borderSpec(sourceBorderSpec, devicePixelRatio)
}
