#if os(iOS)
import SwiftUI
import UIKit

/// Native edge recognition cancels a row's touch when navigation begins.
/// A simultaneous SwiftUI drag can otherwise also activate that row on iPad.
struct NavigationEdgeGesture: UIViewRepresentable {
    let enabled: Bool
    let navigate: () -> Void

    func makeUIView(context: Context) -> EdgeObserverView { EdgeObserverView() }

    func updateUIView(_ view: EdgeObserverView, context: Context) {
        view.navigate = navigate
        view.edgePan.isEnabled = enabled
    }

    static func dismantleUIView(_ view: EdgeObserverView, coordinator: ()) {
        view.detach()
    }

    final class EdgeObserverView: UIView, UIGestureRecognizerDelegate {
        var navigate: () -> Void = {}
        private weak var observedWindow: UIWindow?
        lazy var edgePan: UIScreenEdgePanGestureRecognizer = {
            let recognizer = UIScreenEdgePanGestureRecognizer(target: self, action: #selector(handle(_:)))
            recognizer.edges = .left
            recognizer.cancelsTouchesInView = true
            recognizer.delegate = self
            return recognizer
        }()

        override func didMoveToWindow() {
            super.didMoveToWindow()
            guard observedWindow !== window else { return }
            detach()
            observedWindow = window
            window?.addGestureRecognizer(edgePan)
        }

        func detach() {
            observedWindow?.removeGestureRecognizer(edgePan)
            observedWindow = nil
        }

        override func gestureRecognizerShouldBegin(_ gestureRecognizer: UIGestureRecognizer) -> Bool {
            // Sheets own their own navigation & dismissal gestures.
            observedWindow?.rootViewController?.presentedViewController == nil
        }

        func gestureRecognizer(_ gestureRecognizer: UIGestureRecognizer,
                               shouldBeRequiredToFailBy otherGestureRecognizer: UIGestureRecognizer) -> Bool {
            otherGestureRecognizer is UIPanGestureRecognizer
        }

        @objc private func handle(_ recognizer: UIScreenEdgePanGestureRecognizer) {
            guard recognizer.state == .ended else { return }
            let offset = recognizer.translation(in: observedWindow)
            guard offset.x > 60, offset.x > abs(offset.y) * 1.8 else { return }
            navigate()
        }
    }
}
#endif
