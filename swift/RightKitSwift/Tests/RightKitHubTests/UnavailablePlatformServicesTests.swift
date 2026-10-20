import Foundation
import Testing
import RightKitHub

@Test func unavailablePlatformServicesExposeTypedRefusalForEveryAction() async throws {
    let services = UnavailablePlatformServices()
    await #expect(throws: HubPlatformError.unsupported("revealFile")) { try await services.revealFile("/invalid/probe") }
    await #expect(throws: HubPlatformError.unsupported("openURL")) { try await services.openURL(URL(string: "https://invalid.example")!) }
    await #expect(throws: HubPlatformError.unsupported("moveToTrash")) { try await services.moveToTrash("/invalid/probe") }
    await #expect(throws: HubPlatformError.unsupported("requestPermission")) { _ = try await services.requestPermission("probe") }
    await #expect(throws: HubPlatformError.unsupported("readClipboardText")) { _ = try await services.readClipboardText() }
    await #expect(throws: HubPlatformError.unsupported("writeClipboardText")) { try await services.writeClipboardText("probe") }
    await #expect(throws: HubPlatformError.unsupported("postNotification")) { try await services.postNotification(.init(title: "Probe")) }
    await #expect(throws: HubPlatformError.unsupported("setTrayPresence")) { try await services.setTrayPresence(nil) }
    await #expect(throws: HubPlatformError.unsupported("isLaunchAtLoginEnabled")) { _ = try await services.isLaunchAtLoginEnabled() }
    await #expect(throws: HubPlatformError.unsupported("setLaunchAtLogin")) { try await services.setLaunchAtLogin(true) }
    await #expect(throws: HubPlatformError.unsupported("claimSingleInstance")) { _ = try await services.claimSingleInstance("probe") }
    await #expect(throws: HubPlatformError.unsupported("chooseFolder")) { _ = try await services.chooseFolder(prompt: "Probe", startingAt: nil) }
    #expect(throws: HubPlatformError.unsupported("registerFont")) { _ = try services.registerFont(Data()) }
    #expect(await services.permissionStatus("probe") == .unsupported)
    #expect(await services.decodeImage(Data(), maxPixel: 1) == nil)
    var tray = services.trayActivations.makeAsyncIterator()
    let next = await tray.next()
    #expect(next == nil)
}
