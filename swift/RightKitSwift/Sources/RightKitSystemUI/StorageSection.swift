#if os(macOS)
import RightKitHub
import SwiftUI

/// Storage: port of `hub/src/views/Storage.tsx` and the panels it embeds (Duplicates, Drive health,
/// Chrome snapshots). The model and views live in `Storage/`.
struct StorageSection: PulseSection {
    let notch: any NotchBridge

    let id = "storage"
    let title = "Storage"
    let symbol = "internaldrive"
    let order = 20
    var searchEntries: [HubSearchEntry] {
        [
            HubSearchEntry(id: "storage", title: "Storage", keywords: ["disk", "files", "volumes", "folders", "scan", "space"]),
            HubSearchEntry(id: "findings", title: "Findings", detail: "What is safe to clear", keywords: ["clean", "free", "trash", "cache"]),
            HubSearchEntry(id: "duplicates", title: "Duplicates", detail: "Exact-content copies", keywords: ["copies", "same files"]),
            HubSearchEntry(id: "drive-health", title: "Drive health", detail: "SMART temperature and wear", keywords: ["smart", "ssd", "disk"]),
            HubSearchEntry(id: "eject", title: "Eject mounted installers", keywords: ["dmg", "disk image"]),
        ]
    }

    @MainActor func makeView(_ ctx: HubContext) -> AnyView {
        AnyView(StorageRootView(ctx: ctx, notch: notch))
    }

    @MainActor func preparedView(_ ctx: HubContext) async -> AnyView {
        let model = StorageModel(ctx: ctx, notch: notch)
        await model.load()
        return AnyView(StorageRootView(model: model))
    }

    /// The inspector with an image selected, for the picture of the preview: the Folders workspace
    /// opened on a folder of pictures with one selected, its preview read and decoded.
    @MainActor func preparedStates(_ ctx: HubContext) async -> [SectionState] {
        let model = StorageModel(ctx: ctx, notch: notch)
        await model.load()
        model.choosePane(.folders)
        await model.openAndWait("/Users/demo/Pictures")
        model.select("/Users/demo/Pictures/Harbor at dawn.png")
        await model.settle()
        return [SectionState(name: "preview", view: AnyView(StorageRootView(model: model)))]
    }
}
#endif
