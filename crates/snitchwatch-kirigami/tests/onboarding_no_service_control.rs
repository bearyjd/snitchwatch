//! The onboarding page observes the authenticated bridge connection and
//! cannot fix an unreachable bridge (it is socket-activated, and the shipped
//! Flatpak has no host systemd access), so it must never control services.
//! Issue #384.

const FILES: &[(&str, &str)] = &[
    (
        "qml/OnboardingPage.qml",
        include_str!("../qml/OnboardingPage.qml"),
    ),
    ("src/wizard.rs", include_str!("../src/wizard.rs")),
    (
        "src/wizard_controller.rs",
        include_str!("../src/wizard_controller.rs"),
    ),
];

#[test]
fn onboarding_never_controls_services_or_points_at_the_retired_layering_doc() {
    for (name, source) in FILES {
        for forbidden in ["systemctl", "startUnit", "rpm-ostree-layering"] {
            assert!(
                !source.contains(forbidden),
                "{name} must not mention `{forbidden}`"
            );
        }
    }
}
