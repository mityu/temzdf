// termctl: drive a GUI terminal window for automated checks on macOS. Built and
// called by scripts/macos-gui.sh; see doc/macos.md.
//
//   termctl windows PID                  "WID X Y W H TITLE" per window (global points)
//   termctl move PID X Y [W H]           move (and resize) PID's windows (accessibility API)
//   termctl activate PID                 bring PID to the front; prints the previous front PID
//   termctl key PID [--global] KEY...    keys: `j`, `G`, `/`, `C-f`, `S-Tab`, `M-Left`, `Return`, ...
//   termctl type PID [--global] TEXT     printable ASCII, typed with US-layout key codes
//   termctl click PID X Y [ctrl|shift|cmd]
//   termctl drag PID X1 Y1 X2 Y2
//   termctl scroll PID X Y STEPS [ctrl]  STEPS > 0 scrolls down, one wheel line per step
//
// Mouse coordinates are points relative to the window's top-left corner,
// title bar included (as in `screencapture -l -o` scaled to points). Mouse
// events go to whatever window is under the pointer, so the mouse commands
// activate PID first and give the front back to the previous app afterwards.
//
// Keys are posted to PID's process only, never to another window. Some apps
// (WezTerm) ignore such events: --global activates PID, checks it is frontmost
// and posts the keys globally. Ghostty needs real key codes, hence the US map.

import AppKit
import ApplicationServices
import CoreGraphics
import Foundation

let args = CommandLine.arguments
/// The app to give the front back to, set while a command holds it.
var restoreFront: pid_t = 0
func fail(_ msg: String) -> Never {
    FileHandle.standardError.write((msg + "\n").data(using: .utf8)!)
    giveFrontBack()
    exit(1)
}
func num(_ i: Int) -> Double {
    guard i < args.count, let v = Double(args[i]) else { fail("expected a number at argument \(i)") }
    return v
}
guard args.count >= 3, let pid = pid_t(args[2]) else {
    fail("usage: termctl COMMAND PID ... (see the header of scripts/macos/termctl.swift)")
}

func settle() { usleep(20000) }

/// Sleep while running the run loop, which keeps NSWorkspace's view of the
/// frontmost app current.
func wait(_ seconds: Double) {
    RunLoop.current.run(until: Date().addingTimeInterval(seconds))
}

struct Win {
    let id: Int
    let bounds: CGRect
    let title: String
}

func windows(_ pid: pid_t) -> [Win] {
    let info = CGWindowListCopyWindowInfo([.optionOnScreenOnly], kCGNullWindowID) as? [[String: Any]] ?? []
    return info.compactMap { w in
        guard (w[kCGWindowOwnerPID as String] as? pid_t) == pid,
            (w[kCGWindowLayer as String] as? Int) == 0,
            let id = w[kCGWindowNumber as String] as? Int,
            let b = w[kCGWindowBounds as String] as? [String: Double]
        else { return nil }
        let rect = CGRect(x: b["X"]!, y: b["Y"]!, width: b["Width"]!, height: b["Height"]!)
        return Win(id: id, bounds: rect, title: w[kCGWindowName as String] as? String ?? "")
    }
}

func frontPid() -> pid_t { NSWorkspace.shared.frontmostApplication?.processIdentifier ?? 0 }

func activate(_ pid: pid_t) {
    guard let app = NSRunningApplication(processIdentifier: pid) else { fail("no app with pid \(pid)") }
    app.activate()
    for _ in 0..<150 where frontPid() != pid { wait(0.02) }
    guard frontPid() == pid else { fail("could not bring \(pid) to the front") }
    wait(0.4)
}

func giveFrontBack() {
    guard restoreFront != 0, let app = NSRunningApplication(processIdentifier: restoreFront) else { return }
    restoreFront = 0
    app.activate()
    for _ in 0..<150 where frontPid() != app.processIdentifier { wait(0.02) }
}

/// Run `body` with PID in front, then give the front back (also when PID
/// could not be activated).
func withFront(_ pid: pid_t, _ body: () -> Void) {
    let previous = frontPid()
    if previous != pid && previous != 0 { restoreFront = previous }
    if previous != pid { activate(pid) }
    body()
    // Let the app take the events before it stops being active.
    wait(0.3)
    giveFrontBack()
}

// MARK: keys

// US ANSI virtual key codes (Carbon kVK_ANSI_*) for unshifted and shifted characters.
let plain: [Character: CGKeyCode] = [
    "a": 0, "s": 1, "d": 2, "f": 3, "h": 4, "g": 5, "z": 6, "x": 7, "c": 8, "v": 9, "b": 11, "q": 12,
    "w": 13, "e": 14, "r": 15, "y": 16, "t": 17, "1": 18, "2": 19, "3": 20, "4": 21, "6": 22, "5": 23,
    "=": 24, "9": 25, "7": 26, "-": 27, "8": 28, "0": 29, "]": 30, "o": 31, "u": 32, "[": 33, "i": 34,
    "p": 35, "l": 37, "j": 38, "'": 39, "k": 40, ";": 41, "\\": 42, ",": 43, "/": 44, "n": 45, "m": 46,
    ".": 47, " ": 49, "`": 50,
]
let shifted: [Character: Character] = [
    "!": "1", "@": "2", "#": "3", "$": "4", "%": "5", "^": "6", "&": "7", "*": "8", "(": "9", ")": "0",
    "_": "-", "+": "=", "{": "[", "}": "]", "|": "\\", ":": ";", "\"": "'", "<": ",", ">": ".", "?": "/",
    "~": "`",
]
let named: [String: CGKeyCode] = [
    "Return": 36, "Enter": 36, "Tab": 48, "Space": 49, "BackSpace": 51, "Backspace": 51, "Escape": 53,
    "Esc": 53, "Left": 123, "Right": 124, "Down": 125, "Up": 126, "Home": 115, "End": 119,
    "PageUp": 116, "PageDown": 121, "Delete": 117,
]

/// Key code and flags for one character, or nil if it has no US-layout key.
func charKey(_ c: Character) -> (CGKeyCode, CGEventFlags)? {
    if let code = plain[c] { return (code, []) }
    if c.isUppercase, let code = plain[Character(c.lowercased())] { return (code, .maskShift) }
    if let base = shifted[c], let code = plain[base] { return (code, .maskShift) }
    return nil
}

/// Parse `C-S-M-key` (Ctrl, Shift, Option) into a key code, flags and the text it types.
func parseKey(_ spec: String) -> (CGKeyCode, CGEventFlags, String?) {
    var flags: CGEventFlags = []
    var rest = Substring(spec)
    while rest.count > 2, rest.dropFirst().first == "-" {
        switch rest.first! {
        case "C": flags.insert(.maskControl)
        case "S": flags.insert(.maskShift)
        case "M": flags.insert(.maskAlternate)
        default: fail("unknown modifier in \(spec)")
        }
        rest = rest.dropFirst(2)
    }
    if let code = named[String(rest)] { return (code, flags, nil) }
    guard rest.count == 1, let (code, f) = charKey(rest.first!) else { fail("unknown key \(spec)") }
    return (code, flags.union(f), flags.isEmpty ? String(rest) : nil)
}

func post(_ e: CGEvent, global: Bool) {
    if global {
        guard frontPid() == pid else { fail("\(pid) is no longer frontmost; key not sent") }
        e.post(tap: .cghidEventTap)
    } else {
        e.postToPid(pid)
    }
    settle()
}

func postKey(_ code: CGKeyCode, _ flags: CGEventFlags, _ text: String?, global: Bool) {
    for down in [true, false] {
        guard let e = CGEvent(keyboardEventSource: nil, virtualKey: code, keyDown: down) else { continue }
        e.flags = flags
        if let text {
            let u = Array(text.utf16)
            e.keyboardSetUnicodeString(stringLength: u.count, unicodeString: u)
        }
        post(e, global: global)
    }
}

func sendKeys(_ keys: [(CGKeyCode, CGEventFlags, String?)], global: Bool) {
    if global {
        withFront(pid) { for k in keys { postKey(k.0, k.1, k.2, global: true) } }
    } else {
        for k in keys { postKey(k.0, k.1, k.2, global: false) }
    }
}

// MARK: mouse

func origin() -> CGPoint {
    guard let w = windows(pid).first else { fail("\(pid) has no window") }
    return w.bounds.origin
}

func mouse(_ type: CGEventType, _ p: CGPoint, _ flags: CGEventFlags = []) {
    let e = CGEvent(mouseEventSource: nil, mouseType: type, mouseCursorPosition: p, mouseButton: .left)
    e?.flags = flags
    e?.post(tap: .cghidEventTap)
    settle()
}

func modifier(_ i: Int) -> CGEventFlags {
    guard i < args.count else { return [] }
    switch args[i] {
    case "ctrl": return .maskControl
    case "shift": return .maskShift
    case "cmd": return .maskCommand
    default: fail("unknown modifier \(args[i])")
    }
}

// MARK: commands

switch args[1] {
case "windows":
    for w in windows(pid) {
        let b = w.bounds
        print(w.id, Int(b.minX), Int(b.minY), Int(b.width), Int(b.height), w.title)
    }
case "move":
    let app = AXUIElementCreateApplication(pid)
    var value: CFTypeRef?
    AXUIElementCopyAttributeValue(app, kAXWindowsAttribute as CFString, &value)
    for w in (value as? [AXUIElement]) ?? [] {
        var p = CGPoint(x: num(3), y: num(4))
        AXUIElementSetAttributeValue(w, kAXPositionAttribute as CFString, AXValueCreate(.cgPoint, &p)!)
        if args.count > 6 {
            var s = CGSize(width: num(5), height: num(6))
            AXUIElementSetAttributeValue(w, kAXSizeAttribute as CFString, AXValueCreate(.cgSize, &s)!)
        }
    }
case "activate":
    print(frontPid())
    activate(pid)
case "key", "type":
    var rest = Array(args.dropFirst(3))
    let global = rest.first == "--global"
    if global { rest.removeFirst() }
    let keys: [(CGKeyCode, CGEventFlags, String?)]
    if args[1] == "key" {
        keys = rest.map(parseKey)
    } else {
        keys = rest.joined(separator: " ").map { c in
            guard let (code, f) = charKey(c) else { fail("cannot type \(c)") }
            return (code, f, String(c))
        }
    }
    sendKeys(keys, global: global)
case "click":
    let o = origin()
    let p = CGPoint(x: o.x + num(3), y: o.y + num(4))
    let f = modifier(5)
    withFront(pid) {
        mouse(.mouseMoved, p)
        mouse(.leftMouseDown, p, f)
        mouse(.leftMouseUp, p, f)
    }
case "drag":
    let o = origin()
    let a = CGPoint(x: o.x + num(3), y: o.y + num(4))
    let b = CGPoint(x: o.x + num(5), y: o.y + num(6))
    withFront(pid) {
        mouse(.mouseMoved, a)
        mouse(.leftMouseDown, a)
        for i in 1...20 {
            let t = Double(i) / 20
            mouse(.leftMouseDragged, CGPoint(x: a.x + (b.x - a.x) * t, y: a.y + (b.y - a.y) * t))
        }
        mouse(.leftMouseUp, b)
    }
case "scroll":
    let o = origin()
    let p = CGPoint(x: o.x + num(3), y: o.y + num(4))
    let steps = Int(num(5))
    let f = modifier(6)
    withFront(pid) {
        mouse(.mouseMoved, p)
        for _ in 0..<abs(steps) {
            let e = CGEvent(
                scrollWheelEvent2Source: nil, units: .line, wheelCount: 1, wheel1: steps > 0 ? -1 : 1, wheel2: 0,
                wheel3: 0)
            e?.location = p
            e?.flags = f
            e?.post(tap: .cghidEventTap)
            settle()
        }
    }
default:
    fail("unknown command \(args[1])")
}
