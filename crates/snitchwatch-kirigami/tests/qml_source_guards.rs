//! Source-level invariants for the verdict click path in `ConnectionsPage.qml`.
//!
//! These are deliberately crude text assertions, and they exist because the
//! two richer harnesses each miss this:
//!
//!   * The cxx-qt probes (`inline_verdict_qml.rs`) drive `submitInlineVerdict`
//!     directly — cxx-qt-lib exposes no way to synthesise a mouse event, so
//!     they never exercise a Button's handlers at all.
//!   * `tests/qml/tst_delegate_input.qml` *can* synthesise a real click, but
//!     `qmltestrunner` cannot load `com.snitchwatch.shell` (cxx-qt links those
//!     types statically; there is no QML plugin on disk), so it tests a
//!     structural mirror rather than the real page.
//!
//! That leaves a gap exactly where a regression already happened once, so it
//! gets a cheap guard rather than no guard.

const CONNECTIONS_PAGE: &str = include_str!("../qml/ConnectionsPage.qml");

/// Drop whole-line `//` comments so a guard can't trip over prose that merely
/// *names* the thing it forbids — the doc comments below deliberately discuss
/// `TapHandler`, and an unfiltered substring scan flags itself.
///
/// Only lines whose trimmed form starts with `//` are removed, so no code line
/// is ever touched (a trailing comment after code keeps its code intact, and a
/// `//` inside a string literal is left alone).
fn code_lines(source: &str) -> String {
    source
        .lines()
        .filter(|line| !line.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// A `TapHandler` alongside `onClicked` on a `Controls.Button` dispatches
/// TWICE for a single click — measured with QtTest `mouseClick()` against this
/// delegate's exact shape, identical on the Basic/Fusion/Material/Universal
/// styles (click logic lives in the shared `QQuickAbstractButton` base, not in
/// a style). `tests/qml/tst_delegate_input.qml` pins that measurement.
///
/// The verdict buttons carried both for a while, on the theory that a nested
/// action needs its own pointer grab to beat the delegate. That was true of the
/// original `Controls.ItemDelegate` root, but the delegate is now a plain
/// `Item` with the row's `MouseArea` at `z: 0` beneath the content — plain
/// `onClicked` wins on its own, and the second handler only bought a
/// double-submit of every pending row under a process group.
#[test]
fn verdict_buttons_carry_no_redundant_tap_handler() {
    assert!(
        !code_lines(CONNECTIONS_PAGE).contains("TapHandler"),
        "ConnectionsPage.qml gained a TapHandler. On a Controls.Button that \
         double-dispatches with onClicked (one click -> two handler runs), which \
         previously let \"Allow all\" submit every pending verdict twice. The row's \
         MouseArea sits at z:0 below the content, so onClicked already wins the \
         grab — no extra pointer handler is needed. If you genuinely need one, \
         remove the sibling onClicked and update tests/qml/tst_delegate_input.qml."
    );
}

/// Every verdict button must dispatch through `row.decideOnce(...)`, which owns
/// the `row.submitted` latch. When the guard was written inline at each call
/// site instead, two of the four sites silently lacked it.
#[test]
fn verdict_buttons_all_dispatch_through_the_single_guard() {
    let code = code_lines(CONNECTIONS_PAGE);
    let dispatches = code.matches("onClicked: row.decideOnce(").count();
    assert_eq!(
        dispatches, 4,
        "expected exactly 4 verdict buttons dispatching via row.decideOnce() \
         (inline Allow/Deny + batch Allow all/Deny all), found {dispatches}. If a \
         button was added or removed, update this count; if one now dispatches \
         some other way, it has bypassed the re-entry guard."
    );

    let latches = code.matches("row.submitted = true").count();
    assert_eq!(
        latches, 1,
        "`row.submitted = true` must be set in exactly one place (inside \
         decideOnce), found {latches}. A second latch site means the re-entry \
         guard has been duplicated, which is how it previously ended up missing \
         from the batch handlers."
    );
}
