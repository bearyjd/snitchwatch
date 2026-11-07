//! A bounded separate-process smoke target: Qt permits one app on main.
// Isolates HOME and the XDG dirs before `main` (tests/common/mod.rs).
#[allow(dead_code)]
mod common;

fn main() {
    std::env::set_var("QT_QPA_PLATFORM", "offscreen");
    assert!(snitchwatch_kirigami::application::widget_smoke());
    println!("QApplication/widget/argc-argv-lifetime smoke PASS");
}
