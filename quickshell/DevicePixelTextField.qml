import qs.Commons
import qs.Ui

// TextField backed by a crisp surface instead of the default control fill.
TextField {
  id: root

  property real devicePixelRatio: 1

  background: DevicePixelBorderSurface {
    color: Style.controlFill(root._focused, root._hot, root.foreground,
      root.accent)
    sourceBorderSpec: root._borderSpec
    devicePixelRatio: root.devicePixelRatio
    radius: Style.cornerRadius
  }
}
