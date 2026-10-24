//! The shipped QML and source text the honest-UI guards scan, shared by every guard file.

pub const BLOCKLISTS_PAGE: &str = include_str!("../../qml/BlocklistsPage.qml");
pub const PROFILES_PAGE: &str = include_str!("../../qml/ProfilesPage.qml");
pub const RECOMMENDED_PAGE: &str = include_str!("../../qml/RecommendedRulesPage.qml");
pub const RULES_PAGE: &str = concat!(
    include_str!("../../qml/RulesPage.qml"),
    "\n",
    include_str!("../../qml/RulesInspectorSheet.qml"),
);
pub const SIMULATOR_SHEET: &str = include_str!("../../qml/RuleSimulatorSheet.qml");
pub const IMPORT_SHEET: &str = include_str!("../../qml/RulesImportSheet.qml");
pub const EDITOR_SHEET: &str = include_str!("../../qml/RuleEditorSheet.qml");
pub const PENDING_SHEET: &str = include_str!("../../qml/PendingDecisionSheet.qml");
pub const MAKE_RULE_SHEET: &str = include_str!("../../qml/MakeRuleSheet.qml");
pub const MAKE_RULE_OUTCOMES: &str = include_str!("../../qml/MakeRuleOutcomes.qml");
pub const MAKE_RULE_CONTROLLER_RS: &str = include_str!("../../src/make_rule_controller.rs");
/// The Connections page and the components split out of it (monitor-tree step
/// 1): guards that used to scan the single page file scan the union, so moving
/// code between these files never silently drops a check.
pub const CONNECTIONS_PAGE: &str = concat!(
    include_str!("../../qml/ConnectionsPage.qml"),
    "\n",
    include_str!("../../qml/ConnectionRow.qml"),
    "\n",
    include_str!("../../qml/ConnectionsHeader.qml"),
    "\n",
    include_str!("../../qml/ConnectionsFilterBar.qml"),
    "\n",
    include_str!("../../qml/ConnectionsTopBar.qml"),
    "\n",
    include_str!("../../qml/InspectorState.qml"),
    "\n",
    include_str!("../../qml/HoverTip.qml"),
    "\n",
    include_str!("../../qml/ProgramLabel.qml"),
    "\n",
    include_str!("../../qml/InspectorPanel.qml"),
    "\n",
    include_str!("../../qml/ConnectionDetailPage.qml"),
    "\n",
    include_str!("../../qml/ConnectionList.qml"),
    "\n",
    include_str!("../../qml/RuleButton.qml"),
    "\n",
    include_str!("../../qml/LevelRuleActions.qml"),
    "\n",
    include_str!("../../qml/MapBand.qml"),
    "\n",
    include_str!("../../qml/MapView.qml"),
    "\n",
    include_str!("../../qml/MapMarker.qml"),
    "\n",
    include_str!("../../qml/MonitorPane.qml"),
);
pub const MAIN_QML: &str = include_str!("../../qml/main.qml");
pub const SIZED_SHEET: &str = include_str!("../../qml/SizedOverlaySheet.qml");

/// Every QML file the shell ships, for the guards that must hold everywhere
/// (checked against the directory by `all_qml_files_are_covered`).
pub const ALL_QML: &[(&str, &str)] = &[
    ("AlertHost.qml", include_str!("../../qml/AlertHost.qml")),
    ("AlertWindow.qml", include_str!("../../qml/AlertWindow.qml")),
    ("Durations.qml", include_str!("../../qml/Durations.qml")),
    (
        "ScreenshotRunner.qml",
        include_str!("../../qml/ScreenshotRunner.qml"),
    ),
    (
        "SegmentedChoice.qml",
        include_str!("../../qml/SegmentedChoice.qml"),
    ),
    (
        "SilentModeChoice.qml",
        include_str!("../../qml/SilentModeChoice.qml"),
    ),
    (
        "SilentModePage.qml",
        include_str!("../../qml/SilentModePage.qml"),
    ),
    ("SilentModes.qml", include_str!("../../qml/SilentModes.qml")),
    ("BlocklistsPage.qml", BLOCKLISTS_PAGE),
    (
        "ConnectionsPage.qml",
        include_str!("../../qml/ConnectionsPage.qml"),
    ),
    (
        "ConnectionRow.qml",
        include_str!("../../qml/ConnectionRow.qml"),
    ),
    (
        "ConnectionsHeader.qml",
        include_str!("../../qml/ConnectionsHeader.qml"),
    ),
    (
        "ConnectionsFilterBar.qml",
        include_str!("../../qml/ConnectionsFilterBar.qml"),
    ),
    (
        "ConnectionsTopBar.qml",
        include_str!("../../qml/ConnectionsTopBar.qml"),
    ),
    (
        "InspectorState.qml",
        include_str!("../../qml/InspectorState.qml"),
    ),
    (
        "LearningBanner.qml",
        include_str!("../../qml/LearningBanner.qml"),
    ),
    (
        "LearningLimitsList.qml",
        include_str!("../../qml/LearningLimitsList.qml"),
    ),
    (
        "LearningProgressList.qml",
        include_str!("../../qml/LearningProgressList.qml"),
    ),
    (
        "LearningReviewPage.qml",
        include_str!("../../qml/LearningReviewPage.qml"),
    ),
    (
        "LearningReviewRow.qml",
        include_str!("../../qml/LearningReviewRow.qml"),
    ),
    (
        "LearningStartPanel.qml",
        include_str!("../../qml/LearningStartPanel.qml"),
    ),
    (
        "LearningSummaryDialog.qml",
        include_str!("../../qml/LearningSummaryDialog.qml"),
    ),
    ("HoverTip.qml", include_str!("../../qml/HoverTip.qml")),
    (
        "ProgramLabel.qml",
        include_str!("../../qml/ProgramLabel.qml"),
    ),
    ("StackNav.qml", include_str!("../../qml/StackNav.qml")),
    (
        "InspectorPanel.qml",
        include_str!("../../qml/InspectorPanel.qml"),
    ),
    (
        "ConnectionDetailPage.qml",
        include_str!("../../qml/ConnectionDetailPage.qml"),
    ),
    (
        "ConnectionList.qml",
        include_str!("../../qml/ConnectionList.qml"),
    ),
    (
        "DecideLaterButton.qml",
        include_str!("../../qml/DecideLaterButton.qml"),
    ),
    ("RuleButton.qml", include_str!("../../qml/RuleButton.qml")),
    ("MapBand.qml", include_str!("../../qml/MapBand.qml")),
    (
        "MapBandState.qml",
        include_str!("../../qml/MapBandState.qml"),
    ),
    ("MapMarker.qml", include_str!("../../qml/MapMarker.qml")),
    ("MapView.qml", include_str!("../../qml/MapView.qml")),
    ("MonitorPane.qml", include_str!("../../qml/MonitorPane.qml")),
    (
        "LevelRuleActions.qml",
        include_str!("../../qml/LevelRuleActions.qml"),
    ),
    (
        "DaemonHealthPage.qml",
        include_str!("../../qml/DaemonHealthPage.qml"),
    ),
    (
        "DiagnosticsPage.qml",
        include_str!("../../qml/DiagnosticsPage.qml"),
    ),
    (
        "CuratedGroupsPage.qml",
        include_str!("../../qml/CuratedGroupsPage.qml"),
    ),
    (
        "CuratedGroupsStep.qml",
        include_str!("../../qml/CuratedGroupsStep.qml"),
    ),
    (
        "DnsConsentDialog.qml",
        include_str!("../../qml/DnsConsentDialog.qml"),
    ),
    ("GeoPage.qml", include_str!("../../qml/GeoPage.qml")),
    (
        "GeoDatabaseSettings.qml",
        include_str!("../../qml/GeoDatabaseSettings.qml"),
    ),
    (
        "InlineVerdicts.qml",
        include_str!("../../qml/InlineVerdicts.qml"),
    ),
    ("main.qml", MAIN_QML),
    ("MakeRuleOutcomes.qml", MAKE_RULE_OUTCOMES),
    (
        "MakeRuleSheet.qml",
        include_str!("../../qml/MakeRuleSheet.qml"),
    ),
    (
        "OnboardingPage.qml",
        include_str!("../../qml/OnboardingPage.qml"),
    ),
    ("PendingDecisionSheet.qml", PENDING_SHEET),
    ("ProfilesPage.qml", PROFILES_PAGE),
    (
        "PromptSlotBanner.qml",
        include_str!("../../qml/PromptSlotBanner.qml"),
    ),
    ("RecommendedRulesPage.qml", RECOMMENDED_PAGE),
    ("RuleEditorSheet.qml", EDITOR_SHEET),
    ("RuleSimulatorSheet.qml", SIMULATOR_SHEET),
    ("RulesImportSheet.qml", IMPORT_SHEET),
    (
        "RulesEmptyPlaceholder.qml",
        include_str!("../../qml/RulesEmptyPlaceholder.qml"),
    ),
    (
        "RulesInsightsText.qml",
        include_str!("../../qml/RulesInsightsText.qml"),
    ),
    ("RulesPage.qml", RULES_PAGE),
    (
        "RulesInspectorSheet.qml",
        include_str!("../../qml/RulesInspectorSheet.qml"),
    ),
    ("ScannerPage.qml", include_str!("../../qml/ScannerPage.qml")),
    ("SizedOverlaySheet.qml", SIZED_SHEET),
    ("TrafficPage.qml", include_str!("../../qml/TrafficPage.qml")),
    ("Theme.qml", include_str!("../../qml/Theme.qml")),
    ("TrayMenu.qml", include_str!("../../qml/TrayMenu.qml")),
];
