import SwiftUI

struct ConnectionDotView: View {
    let online: Bool

    var body: some View {
        Circle()
            .fill(online ? Theme.accent2 : Theme.muted)
            .frame(width: 8, height: 8)
            .accessibilityLabel(online ? "Connected" : "Disconnected")
    }
}
