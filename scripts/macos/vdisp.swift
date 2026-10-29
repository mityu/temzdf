// vdisp [W H [SCALE]]: create a headless virtual display (default 1600x1000
// points at scale 2, HiDPI) and keep it until killed. Windows on it render as
// if visible (kitty stops redrawing covered windows) without covering
// anything on the real screen. Prints "ID X Y W H" (global points) once the
// display is up. Uses the private CGVirtualDisplay API declared in
// CGVirtualDisplay.h. Built and started by scripts/macos-gui.sh (HiDPI) and
// scripts/e2e.sh (scale 1: screenshot pixels are points, as on Linux).

import CoreGraphics
import Foundation

let args = CommandLine.arguments
let width = args.count > 2 ? UInt32(args[1]) ?? 1600 : 1600
let height = args.count > 2 ? UInt32(args[2]) ?? 1000 : 1000
let scale = args.count > 3 ? UInt32(args[3]) ?? 2 : 2

let descriptor = CGVirtualDisplayDescriptor()
descriptor.queue = DispatchQueue.main
descriptor.name = "temzdf-test"
descriptor.maxPixelsWide = width * scale
descriptor.maxPixelsHigh = height * scale
// About 100 points per inch, whatever the size.
descriptor.sizeInMillimeters = CGSize(width: Double(width) / 4, height: Double(height) / 4)
descriptor.productID = 0x7465
descriptor.vendorID = 0x6d7a
descriptor.serialNum = 1
guard let display = CGVirtualDisplay(descriptor: descriptor) else {
    print("cannot create a virtual display")
    exit(1)
}
let settings = CGVirtualDisplaySettings()
settings.hiDPI = scale > 1 ? 1 : 0
settings.modes = [CGVirtualDisplayMode(width: width, height: height, refreshRate: 60)]
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
