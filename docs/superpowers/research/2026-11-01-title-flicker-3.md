# Title-area scroll flicker, third attempt (2026-11-01)

Status: NOT reproduced. No production code changed. A regression probe was
added: `crates/snitchwatch-kirigami/tests/qml/tst_title_area_flicker.qml`.

Owner report: a one-line text with words like "working" / "waiting" flashes
for a few ms, only while scrolling with the pointer over the title area, on
both Daemon Health and Security Scan. Already ruled out: the
`DaemonHealthModel` "Waiting for complete daemon health information" transient
(PR #285, see `2026-10-30-scan-page-flicker-2.md`).

## What changed since attempt 2

Attempt 2 loaded `main.qml` in its startup no-daemon state. There the
prompt-slot banner was hidden, and at 900x640 neither page could scroll. This
attempt changes those conditions:

- The window-level banners in `main.qml` (`bridgeBanner`,
  `daemonHealthBanner`, `pendingExposureBanner`, `PromptSlotBanner`,
  `LearningBanner`) are stacked from the window top, each anchored under the
  visible one before it, with `z: 999` (`LearningBanner` 998). The first one up
  lies on top of the page's title area. Pointer "over the title area" while a banner is
  up means pointer over the banner.
- The probe shows the real `PromptSlotBanner` ("A connection is waiting for
  your answer." plus the holder line "... has been waiting for your answer for
  N s.") with a held slot over the real `ScannerPage` and `DaemonHealthPage`,
  inside a `Kirigami.ApplicationWindow`. The holder text ticks every 250 ms,
  which is faster than the real 1 s `PromptSlotStatus.refresh()`.
- Matrix: width 500/640 by height 300/400 by both pages by "Decide later"
  offered or not. "Decide later" changes the width of the action row, and with
  it Kirigami's `InlineMessage` actions-at-bottom layout choice
  (`templates/InlineMessage.qml` around lines 253-284). The earlier exploratory
  run also covered widths 560 and 760.
- Each case sends 12 wheel events at each of four places: the banner heading
  row, the banner middle, just under the banner (the page header) and the
  body. At 300 px Security Scan overflows, so the wheel really moves it.
- On every `frameSwapped` and every 4 ms it records each text item in the
  window and in `Overlay.overlay`: text, effective visibility, opacity product
  and height, plus y for the banner subtree. Each field is tracked on its own.
  The probe fails when a field leaves the value it has held for 120 ms and
  returns to it within 120 ms, whatever values it passed through on the way,
  provided the item was on screen at some point. That is the flicker's
  signature. An item created mid-run starts from "absent", so one that
  appears and goes again within 120 ms counts too. It also asserts that the banner heading was sampled on screen and,
  for the overflowing Security Scan rows, that the wheel moved `contentY`.

Result: zero transients on Basic, Fusion and Material, and no binding-loop
warnings. Two mutant checks show the detector works. A `WheelHandler` on the
banner that hid its heading for 16 ms per wheel event failed 16 of 18 rows. The
same handler dipping the heading's opacity to 0.4 failed 14 of 18.

## Ruled out (in this sandbox, within the probe's limits)

Limits: the probe samples properties every frame and every 4 ms. It does not
see rendering-only artefacts (a scene-graph glitch with unchanged properties).
Synthetic wheel events also don't fully reproduce a real pointer hovering.

- A wheel- or scroll-driven change of any text, visibility, opacity or
  height on either page, in the prompt-slot banner, or in a tooltip/overlay.
  This covers the probe's sizes and styles and both "Decide later" layouts.
- An `InlineMessage` layout flip (label line count / actions row moving) while
  the holder text ticks.
- "working" as app or Kirigami text: the only user-visible string in `qml/`
  or `src/` with "working" is demo-mode data (`src/demo.rs`, "keep the clock and
  the network working"). Nothing that can show on these two pages has it. `strings` over every Kirigami and QtQuick.Controls
  plugin `.so` in `/usr/lib64/qt6/qml` finds no "working" or "waiting" either.
  Either the word is a misread of the fast flash, or it comes from the
  owner's Breeze style (`org.kde.desktop`, not installable here) or from the
  desktop/compositor.

## Remaining hypotheses

1. **State-driven, not scroll-driven.** `PromptSlotStatus.held` goes true
   then false within one frame or a few, for example a short-lived daemon
   prompt answered or timed out at once. The banner then appears for a
   moment over the title area, wherever the pointer is.
   `PromptSlotStatus` has no log for that flip today.
2. **Breeze-only.** Something in `qqc2-desktop-style` (scrollbar or
   tooltip) reacts to the wheel over the header. It can't be tested here.
3. **Outside the app.** A KWin titlebar wheel action, or a Plasma tooltip on
   the compositor's titlebar.

## What would tell them apart (from the owner)

1. Was a "A connection is waiting for your answer." banner on screen just
   before or after the flicker? Is the flicker in the app's own page header
   (under the compositor titlebar), or in the compositor titlebar itself?
   What window size and scale factor?
2. Does it happen with the pointer still and the wheel untouched? Watch the
   same spot for 30 s. If yes, hypothesis 1. If it happens only with the
   wheel, 2 or 3.
3. A run with repaint highlighting and logs (copy the terminal output):

       flatpak run --env=QSG_VISUALIZE=changes \
         --env=SNITCHWATCH_LOG=snitchwatch_kirigami=debug \
         --env=QT_LOGGING_RULES='qt.qml.binding.removal.info=true;qt.quick.hover.trace=true' \
         org.snitchwatch.Snitchwatch 2>&1 | tee flicker.log

   `QSG_VISUALIZE=changes` paints each repainted region a new random colour
   every frame. A screenshot or phone video taken while scrolling shows which
   item repaints. If the banner's rectangle lights up, hypothesis 1 or 2; if
   nothing in the window repaints, hypothesis 3. The app has no frame-dump
   variable; a phone recording at 240 fps of the `QSG_VISUALIZE` run is the
   closest substitute.
4. The same with `QT_QUICK_CONTROLS_STYLE=Basic` added. If the flicker goes
   away, hypothesis 2.

Next code step if (1) points at hypothesis 1: add a debug log of
`PromptSlotStatus` `held` flips (the way PR #285 did for health), then
look at the timestamps.
