import SwiftUI

struct ContentView: View {
    @EnvironmentObject private var send: SendCoordinator
    @State private var selection = ContentView.initialTab

    var body: some View {
        TabView(selection: $selection) {
            QueueView()
                .tabItem { Label("Queue", systemImage: "tray.full") }
                .tag("queue")
            LibraryView()
                .tabItem { Label("Library", systemImage: "books.vertical") }
                .tag("library")
            ReceiveView()
                .tabItem { Label("Receive", systemImage: "arrow.down.circle") }
                .tag("receive")
            SettingsView()
                .tabItem { Label("Settings", systemImage: "gearshape") }
                .tag("settings")
        }
        .overlay(alignment: .top) {
            if let status = send.status {
                SendStatusBanner(status: status)
                    .padding(.top, 8)
                    .transition(.move(edge: .top).combined(with: .opacity))
            }
        }
        .animation(.easeInOut(duration: 0.2), value: send.status)
        .confirmationDialog(
            send.pending?.title ?? "",
            isPresented: Binding(
                get: { send.pending != nil },
                set: { if !$0 { send.dismissOffer() } }
            ),
            titleVisibility: .visible
        ) {
            ForEach(send.currentChoices) { choice in
                Button(choice.title) { send.choose(choice) }
            }
        }
        .alert(
            "Couldn't process this",
            isPresented: Binding(
                get: { send.notice != nil },
                set: { if !$0 { send.notice = nil } }
            )
        ) {
            Button("OK", role: .cancel) {}
        } message: {
            Text(send.notice ?? "")
        }
    }

    /// Debug builds only: `-ScrapeRightInitialTab receive` opens that tab, so
    /// headless screenshots can reach it without UI scripting.
    private static var initialTab: String {
        #if DEBUG
        UserDefaults.standard.string(forKey: "ScrapeRightInitialTab") ?? "queue"
        #else
        "queue"
        #endif
    }
}

/// Brief progress / result chip for a send.
private struct SendStatusBanner: View {
    let status: SendStatus

    var body: some View {
        HStack(spacing: 8) {
            switch status {
            case .sending(let alias, let fraction):
                ProgressView(value: fraction)
                    .progressViewStyle(.circular)
                Text("Sending to \(alias)")
            case .sent(let alias):
                Image(systemName: "checkmark")
                Text("Sent to \(alias)")
            case .failed(let alias, let message):
                Image(systemName: "exclamationmark.triangle")
                Text("Couldn't send to \(alias): \(message)")
            }
        }
        .font(.srBody(14, weight: .medium))
        .foregroundStyle(color)
        .padding(.horizontal, 14)
        .padding(.vertical, 8)
        .background(Color.srElevated, in: Capsule())
        .overlay(Capsule().stroke(Color.srBorder, lineWidth: 1))
        .accessibilityElement(children: .combine)
    }

    private var color: Color {
        if case .failed = status { return .srDanger }
        return .srText
    }
}
