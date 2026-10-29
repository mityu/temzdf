// termctl: drive a GUI terminal window for automated checks on macOS. Built and
// called by scripts/macos-gui.sh; see doc/macos.md.
//
//   termctl windows PID                  "WID X Y W H TITLE" per window (global points)
//   termctl content PID                  "X Y W H" of its first window without the title bar
//   termctl move PID X Y [W H]           move (and resize) PID's windows (accessibility API)
//   termctl front                        the front app's PID
//   termctl main                         "X Y W H SCALE" of the main display (global points)
//   termctl permissions [--ask]          whether this process may drive other apps and take screenshots
//   termctl activate PID                 bring PID to the front; prints the previous front PID
//   termctl key PID [--global] KEY...    keys: `j`, `G`, `/`, `C-f`, `S-Tab`, `M-Left`, `Return`, ...
//   termctl type PID [--global] TEXT     printable ASCII, typed with US-layout key codes
//   termctl click PID X Y [ctrl|shift|cmd]
//   termctl drag PID X1 Y1 X2 Y2
//   termctl scroll PID X Y STEPS [ctrl]  STEPS > 0 scrolls down, one wheel line per step
//   termctl serve PID [pid|global]       commands on stdin, for scripts/e2e.sh (see Server)
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
if args.count == 2 && args[1] == "front" {
    wait(0.05)
    print(frontPid())
    exit(0)
}
if args.count >= 2 && args[1] == "permissions" {
    // Asking adds this process's responsible app to both lists in System
    // Settings (switched off), so that granting is a matter of a switch.
    if args.count > 2 && args[2] == "--ask" {
        _ = AXIsProcessTrustedWithOptions([kAXTrustedCheckOptionPrompt.takeUnretainedValue(): true] as CFDictionary)
        _ = CGRequestScreenCaptureAccess()
    }
    let ax = AXIsProcessTrusted()
    let screen = CGPreflightScreenCaptureAccess()
    print("accessibility: \(ax ? "yes" : "no")")
    print("screen recording: \(screen ? "yes" : "no")")
    exit(ax && screen ? 0 : 1)
}
if args.count == 2 && args[1] == "main" {
    let b = CGDisplayBounds(CGMainDisplayID())
    let scale = NSScreen.screens.first?.backingScaleFactor ?? 1
    print(Int(b.minX), Int(b.minY), Int(b.width), Int(b.height), Int(scale))
    exit(0)
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

/// Ask for `pid` in front: as an app (a request macOS may turn down for a
/// program without a bundle, as temzdf-gui run from the shell), and then
/// through the accessibility API, which it follows.
func requestFront(_ pid: pid_t) {
    NSRunningApplication(processIdentifier: pid)?.activate()
    for _ in 0..<25 where frontPid() != pid { wait(0.02) }
    if frontPid() != pid {
        AXUIElementSetAttributeValue(AXUIElementCreateApplication(pid), kAXFrontmostAttribute as CFString, kCFBooleanTrue)
    }
}

func activate(_ pid: pid_t) {
    guard NSRunningApplication(processIdentifier: pid) != nil else { fail("no app with pid \(pid)") }
    requestFront(pid)
    for _ in 0..<150 where frontPid() != pid { wait(0.02) }
    guard frontPid() == pid else { fail("could not bring \(pid) to the front") }
    wait(0.4)
}

func giveFrontBack() {
    guard restoreFront != 0 else { return }
    let previous = restoreFront
    restoreFront = 0
    requestFront(previous)
    for _ in 0..<150 where frontPid() != previous { wait(0.02) }
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

// MARK: serve (scripts/e2e.sh -d macos)

// XKB keysym names (as xdotool and scripts/e2e.sh's actions take them) of
// characters, and of keys that type none.
let keysymChars: [String: Character] = [
    "space": " ", "plus": "+", "minus": "-", "equal": "=", "slash": "/", "question": "?", "less": "<",
    "greater": ">", "colon": ":", "semicolon": ";", "percent": "%", "comma": ",", "period": ".",
    "apostrophe": "'", "quotedbl": "\"", "exclam": "!", "at": "@", "numbersign": "#", "dollar": "$",
    "asciicircum": "^", "ampersand": "&", "asterisk": "*", "parenleft": "(", "parenright": ")",
    "underscore": "_", "bracketleft": "[", "bracketright": "]", "braceleft": "{", "braceright": "}",
    "backslash": "\\", "bar": "|", "grave": "`", "asciitilde": "~",
]
let keysymKeys: [String: CGKeyCode] = [
    "Return": 36, "Tab": 48, "BackSpace": 51, "Escape": 53, "Delete": 117, "Left": 123, "Right": 124,
    "Down": 125, "Up": 126, "Home": 115, "End": 119, "Prior": 116, "Page_Up": 116, "Next": 121,
    "Page_Down": 121, "F1": 122, "F2": 120, "F3": 99, "F4": 118, "F5": 96, "F6": 97, "F7": 98, "F8": 100,
    "F9": 101, "F10": 109, "F11": 103, "F12": 111,
]
/// Modifier names: their flag and key code (the left one).
let modifierKeys: [String: (CGEventFlags, CGKeyCode)] = [
    "ctrl": (.maskControl, 59), "control": (.maskControl, 59), "shift": (.maskShift, 56),
    "alt": (.maskAlternate, 58), "super": (.maskCommand, 55), "cmd": (.maskCommand, 55),
]

/// An XKB key name, maybe with modifiers (`ctrl+shift+v`, `shift+g`,
/// `percent`, `Return`): key code, flags, and the text it types.
func xkbKey(_ spec: String) -> (CGKeyCode, CGEventFlags, String?)? {
    var parts = spec.split(separator: "+", omittingEmptySubsequences: false).map(String.init)
    let name = parts.removeLast()
    var flags: CGEventFlags = []
    for m in parts {
        guard let (f, _) = modifierKeys[m.lowercased()] else { return nil }
        flags.insert(f)
    }
    if let code = keysymKeys[name] { return (code, flags, nil) }
    let c: Character
    if let named = keysymChars[name] {
        c = named
    } else if name.count == 1 {
        c = name.first!
    } else {
        return nil
    }
    // Shift with a letter types the capital.
    var typed = c
    if flags.contains(.maskShift), let up = String(c).uppercased().first, up != c { typed = up }
    guard let (code, f) = charKey(typed) else { return nil }
    let all = flags.union(f)
    let text = all.intersection([.maskControl, .maskAlternate, .maskCommand]).isEmpty ? String(typed) : nil
    return (code, all, text)
}

/// Serves one command per line on stdin, answering "ok" or an error on
/// stdout: the input device scripts/e2e.sh drives PID with (as wlinput does
/// on Wayland). Pointer events go to the window under the pointer, so they
/// are posted globally with PID in front: the first one brings it there and
/// it stays for the run (giving the front back between actions sent focus
/// events, and WezTerm redraws lazily without the focus); `done` gives the
/// front and the pointer back. Keys go to PID's process (`pid`), or
/// globally with it in front (`global`: WezTerm). Coordinates are global
/// points.
///   move X Y              (a drag while a button is down)
///   down BTN, up BTN      1 left, 2 middle, 3 right
///   wheel N DELAY_MS      N line steps (N > 0 down)
///   key NAME              XKB names: `ctrl+b`, `shift+g`, `percent`, `Return`
///   keydown NAME, keyup NAME  modifiers stay down for later keys and clicks
///   type DELAY_MS TEXT    the rest of the line, typed
///   front                 bring PID to the front now
///   done
final class Server {
    let pid: pid_t
    let globalKeys: Bool
    var flags: CGEventFlags = []
    var pos = CGPoint.zero
    var held: Set<Int> = []
    var holding = false
    var userPointer: CGPoint? = nil
    var lastDown: (time: Date, pos: CGPoint, count: Int64)? = nil

    init(pid: pid_t, globalKeys: Bool) {
        self.pid = pid
        self.globalKeys = globalKeys
    }

    struct Failure: Error { let message: String }

    /// PID in front, from the first event that needs it until `done`.
    func front() throws {
        wait(0.001)
        if frontPid() == pid { return }
        if !holding {
            let previous = frontPid()
            if previous != 0 { restoreFront = previous }
            userPointer = CGEvent(source: nil)?.location
            holding = true
        }
        guard NSRunningApplication(processIdentifier: pid) != nil else { throw Failure(message: "no app \(pid)") }
        requestFront(pid)
        for _ in 0..<150 where frontPid() != pid { wait(0.02) }
        guard frontPid() == pid else { throw Failure(message: "could not bring \(pid) to the front") }
        wait(0.4)
        // Where the actions left the pointer.
        if pos != .zero { postMouse(.mouseMoved, button: .left) }
    }

    func done() {
        giveFrontBack()
        if let p = userPointer { CGWarpMouseCursorPosition(p) }
        holding = false
        userPointer = nil
    }

    func postMouse(_ type: CGEventType, button: CGMouseButton, clicks: Int64 = 1) {
        guard let e = CGEvent(mouseEventSource: nil, mouseType: type, mouseCursorPosition: pos, mouseButton: button)
        else { return }
        e.flags = flags
        e.setIntegerValueField(.mouseEventClickState, value: clicks)
        e.post(tap: .cghidEventTap)
        settle()
    }

    func button(_ n: Int) throws -> CGMouseButton {
        switch n {
        case 1: return .left
        case 2: return .center
        case 3: return .right
        default: throw Failure(message: "bad button \(n)")
        }
    }

    func move(_ p: CGPoint) throws {
        try front()
        pos = p
        if held.contains(1) {
            postMouse(.leftMouseDragged, button: .left)
        } else if held.contains(3) {
            postMouse(.rightMouseDragged, button: .right)
        } else if held.contains(2) {
            postMouse(.otherMouseDragged, button: .center)
        } else {
            postMouse(.mouseMoved, button: .left)
        }
    }

    func press(_ n: Int, down: Bool) throws {
        try front()
        let b = try button(n)
        let types: [Int: (CGEventType, CGEventType)] = [
            1: (.leftMouseDown, .leftMouseUp), 2: (.otherMouseDown, .otherMouseUp), 3: (.rightMouseDown, .rightMouseUp),
        ]
        let (dn, upType) = types[n]!
        // Clicks close together in time and place count up (double clicks).
        var count: Int64 = 1
        if down {
            if let l = lastDown, Date().timeIntervalSince(l.time) < 0.5, abs(l.pos.x - pos.x) < 4,
                abs(l.pos.y - pos.y) < 4
            {
                count = l.count + 1
            }
            lastDown = (Date(), pos, count)
            held.insert(n)
        } else {
            count = lastDown?.count ?? 1
            held.remove(n)
        }
        postMouse(down ? dn : upType, button: b, clicks: count)
    }

    func wheel(_ steps: Int, delayMs: Int) throws {
        try front()
        for i in 0..<abs(steps) {
            if i > 0 { usleep(UInt32(delayMs) * 1000) }
            let line: Int64 = steps > 0 ? -1 : 1
            let e = CGEvent(
                scrollWheelEvent2Source: nil, units: .line, wheelCount: 1, wheel1: Int32(line), wheel2: 0,
                wheel3: 0)
            // One notch of a wheel without acceleration: the system's scroll
            // speed setting scales the deltas of posted line events too.
            e?.setIntegerValueField(.scrollWheelEventDeltaAxis1, value: line)
            e?.setDoubleValueField(.scrollWheelEventFixedPtDeltaAxis1, value: Double(line))
            e?.setIntegerValueField(.scrollWheelEventPointDeltaAxis1, value: line * 10)
            e?.setIntegerValueField(.scrollWheelEventIsContinuous, value: 0)
            e?.location = pos
            e?.flags = flags
            e?.post(tap: .cghidEventTap)
            settle()
        }
    }

    func postKeyEvent(_ code: CGKeyCode, _ f: CGEventFlags, _ text: String?, down: Bool) throws {
        if globalKeys { try front() }
        guard let e = CGEvent(keyboardEventSource: nil, virtualKey: code, keyDown: down) else { return }
        e.flags = f
        if let text {
            let u = Array(text.utf16)
            e.keyboardSetUnicodeString(stringLength: u.count, unicodeString: u)
        }
        if globalKeys { e.post(tap: .cghidEventTap) } else { e.postToPid(pid) }
        settle()
    }

    func modifier(_ name: String, down: Bool) throws -> Bool {
        guard let (f, code) = modifierKeys[name.lowercased()] else { return false }
        if down { flags.insert(f) } else { flags.remove(f) }
        if globalKeys { try front() }
        guard let e = CGEvent(keyboardEventSource: nil, virtualKey: code, keyDown: down) else { return true }
        e.type = .flagsChanged
        e.flags = flags
        if globalKeys { e.post(tap: .cghidEventTap) } else { e.postToPid(pid) }
        settle()
        return true
    }

    func key(_ name: String, down: Bool?) throws {
        if let down, try modifier(name, down: down) { return }
        guard let (code, f, text) = xkbKey(name) else { throw Failure(message: "unknown key \(name)") }
        let all = f.union(flags)
        let typed = all.intersection([.maskControl, .maskAlternate, .maskCommand]).isEmpty ? text : nil
        for d in down.map({ [$0] }) ?? [true, false] { try postKeyEvent(code, all, typed, down: d) }
    }

    func type(_ text: String, delayMs: Int) throws {
        for (i, c) in text.enumerated() {
            if i > 0 && delayMs > 0 { usleep(UInt32(delayMs) * 1000) }
            if let (code, f) = charKey(c) {
                for d in [true, false] { try postKeyEvent(code, f.union(flags), String(c), down: d) }
            } else {
                // No US key types it: the text alone.
                for d in [true, false] { try postKeyEvent(0, flags, String(c), down: d) }
            }
        }
    }

    func handle(_ line: String) throws {
        let words = line.split(separator: " ", maxSplits: 2, omittingEmptySubsequences: true).map(String.init)
        guard let cmd = words.first else { return }
        func int(_ i: Int) throws -> Int {
            guard i < words.count, let v = Int(words[i]) else { throw Failure(message: "expected a number") }
            return v
        }
        switch cmd {
        case "move":
            let xy = line.split(separator: " ").dropFirst().compactMap { Double($0) }
            guard xy.count == 2 else { throw Failure(message: "move X Y") }
            try move(CGPoint(x: xy[0], y: xy[1]))
        case "down", "up": try press(int(1), down: cmd == "down")
        case "wheel": try wheel(int(1), delayMs: int(2))
        case "key", "keydown", "keyup":
            guard words.count == 2 else { throw Failure(message: "\(cmd) NAME") }
            try key(words[1], down: cmd == "key" ? nil : cmd == "keydown")
        case "type":
            // The text may start with or contain spaces: everything after "type MS ".
            let afterDelay = line.dropFirst(5).drop(while: { $0 != " " }).dropFirst()
            try type(String(afterDelay), delayMs: int(1))
        case "front": try front()
        case "done": done()
        default: throw Failure(message: "unknown command \(cmd)")
        }
    }

    func run() {
        while let line = readLine(strippingNewline: true) {
            do {
                try handle(line)
                print("ok")
            } catch let f as Failure {
                print(f.message)
            } catch {
                print("\(error)")
            }
            fflush(stdout)
        }
        done()
    }
}

// MARK: commands

switch args[1] {
case "serve":
    Server(pid: pid, globalKeys: args.count > 3 && args[3] == "global").run()
case "windows":
    for w in windows(pid) {
        let b = w.bounds
        print(w.id, Int(b.minX), Int(b.minY), Int(b.width), Int(b.height), w.title)
    }
case "content":
    // The first window's inside, without a title bar if it has one (a
    // close button tells: undecorated terminal windows have none). The
    // content view is not in the accessibility tree, and NSWindow's own
    // figure (32 on macOS 26) is not what a winit window gets (28): the
    // buttons sit in the middle of the title bar, which gives its height.
    guard let w = windows(pid).first else { fail("\(pid) has no window") }
    let app = AXUIElementCreateApplication(pid)
    var value: CFTypeRef?
    AXUIElementCopyAttributeValue(app, kAXFocusedWindowAttribute as CFString, &value)
    if value == nil {
        AXUIElementCopyAttributeValue(app, kAXWindowsAttribute as CFString, &value)
        value = (value as? [AXUIElement])?.first
    }
    // Among its children: WezTerm's undecorated window has a hidden one.
    var children: CFTypeRef?
    var close: AXUIElement? = nil
    if let win = value, CFGetTypeID(win) == AXUIElementGetTypeID(),
        AXUIElementCopyAttributeValue(win as! AXUIElement, kAXChildrenAttribute as CFString, &children) == .success
    {
        for c in (children as? [AXUIElement]) ?? [] {
            var sub: CFTypeRef?
            AXUIElementCopyAttributeValue(c, kAXSubroleAttribute as CFString, &sub)
            if (sub as? String) == kAXCloseButtonSubrole as String { close = c }
        }
    }
    var titleBar = 0.0
    if let button = close {
        var p = CGPoint.zero
        var size = CGSize.zero
        var v: CFTypeRef?
        if AXUIElementCopyAttributeValue(button, kAXPositionAttribute as CFString, &v) == .success {
            AXValueGetValue(v as! AXValue, .cgPoint, &p)
        }
        if AXUIElementCopyAttributeValue(button, kAXSizeAttribute as CFString, &v) == .success {
            AXValueGetValue(v as! AXValue, .cgSize, &size)
        }
        titleBar = 2 * (p.y + size.height / 2 - w.bounds.minY)
    }
    let b = w.bounds
    print(Int(b.minX), Int(b.minY + titleBar), Int(b.width), Int(b.height - titleBar))
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
