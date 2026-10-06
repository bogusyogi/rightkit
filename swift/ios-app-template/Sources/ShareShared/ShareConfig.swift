import RightKitImportQueue

/// Compiled into the app and the share extension only (the two RightKitImportQueue consumers).
extension TemplateConfig {
    static func shareIntake() -> ShareIntakeConfiguration {
        ShareIntakeConfiguration(groupID: appGroupID)
    }
}
