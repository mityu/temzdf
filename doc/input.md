# Input: mouse, keyboard and smooth scrolling

## Modes enabled at startup

- `CSI ? 1003 h` any-motion tracking (hover feedback on links); `1002` would be
  enough for drag only.
- `CSI ? 1006 h` SGR encoding, plus `CSI ? 1016 h` SGR-pixel coordinates where
  DECRQM (`CSI ? 1016 $ p`) says it is supported. 1016 only changes the encoding:
  coordinates become pixels relative to the window's top-left.
- `CSI > 31 u` kitty keyboard protocol, all flags (disambiguate, event types incl.
  release, alternate keys, all keys as escapes, associated text). Needed for key
  release and modifier-only events. Query with `CSI ? u` + DA1 as sentinel.
- `CSI ? 2004 h` bracketed paste, `CSI ? 1004 h` focus, `CSI ? 2048 h` in-band
  resize.

Everything is reverted on exit and on panic.

## SGR mouse format

`CSI < b ; x ; y M` (press/motion) or `m` (release). Low bits of `b` select the
button; `+32` motion; `64/65` wheel up/down, `66/67` wheel left/right; modifier
bits `4` Shift, `8` Alt, `16` Ctrl. Ctrl+wheel therefore needs no keyboard
protocol. kitty extends 1016 with a leave-window event (bit 8 set).

## Smooth scrolling

Terminals only deliver discrete wheel steps (trackpad motion and macOS momentum are
already quantised by the terminal). We emulate smoothness:

1. Each wheel tick adds N pixels (a fraction of a page step) to a target offset.
2. A 60 Hz animation eases the displayed offset towards the target
   (`offset += (target - offset) * k`), running only while moving.
3. Ticks arriving in quick succession increase the step (flick acceleration).
4. A direction reversal resets the pending distance.
5. Keep any extra momentum mild: the terminal already applied inertia.

Drag-to-pan with 1016 coordinates gives the truly pixel-precise path.

## Zoom

Pinch gestures are never forwarded. Zoom with Ctrl+wheel (anchored at the
pointer) and keys.
