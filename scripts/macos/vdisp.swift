// vdisp: create a headless virtual display (1600x1000 points, HiDPI) and keep it
// until killed. Windows on it render as if visible (kitty stops redrawing
// covered windows) without covering anything on the real screen. Prints
// "ID X Y W H" (global points) once the display is up. Uses the private
// CGVirtualDisplay API declared in CGVirtualDisplay.h. Built and started by
// scripts/macos-gui.sh.

import CoreGraphics
import Foundation

let descriptor = CGVirtualDisplayDescriptor()
descriptor.queue = DispatchQueue.main
descriptor.name = "temzdf-test"
descriptor.maxPixelsWide = 3200
descriptor.maxPixelsHigh = 2000
descriptor.sizeInMillimeters = CGSize(width: 400, height: 250)
descriptor.productID = 0x7465
descriptor.vendorID = 0x6d7a
descriptor.serialNum = 1
guard let display = CGVirtualDisplay(descriptor: descriptor) else {
    print("cannot create a virtual display")
    exit(1)
}
let settings = CGVirtualDisplaySettings()
settings.hiDPI = 1
settings.modes = [CGVirtualDisplayMode(width: 1600, height: 1000, refreshRate: 60)]
guard display.apply(settings) else {
    print("cannot configure the virtual display")
    exit(1)
}
// The display gets its place in the arrangement a moment later.
DispatchQueue.main.asyncAfter(deadline: .now() + 1.5) {
    let b = CGDisplayBounds(display.displayID)
    print(display.displayID, Int(b.minX), Int(b.minY), Int(b.width), Int(b.height))
    fflush(stdout)
}
dispatchMain()
