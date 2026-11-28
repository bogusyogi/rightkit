#if os(macOS)
import AVFoundation
import Foundation

/// Plays one in-memory sound (WAV or any `AVAudioPlayer` format) on a private serial queue, so the
/// caller never waits on decoding or device start. Each `play()` builds a fresh player, starts it, and
/// keeps it until the next call so playback is not cut short. `log` receives `outcome=started`,
/// `outcome=rejected` (the player declined to start) or `outcome=error` (the data did not decode).
public final class RightKitPillDataSoundPlayer: @unchecked Sendable {
    private let queue: DispatchQueue
    private let data: Data
    private let log: (@Sendable (String) -> Void)?
    private var player: AVAudioPlayer?

    /// `queueLabel` names the serial queue (the host picks it).
    public init(queueLabel: String, data: Data, log: (@Sendable (String) -> Void)?) {
        self.queue = DispatchQueue(label: queueLabel, qos: .userInitiated)
        self.data = data
        self.log = log
    }

    public func play() {
        queue.async { [weak self] in
            self?.playOnQueue()
        }
    }

    private func playOnQueue() {
        do {
            let player = try AVAudioPlayer(data: data)
            player.prepareToPlay()
            let started = player.play()
            self.player = player
            log?("outcome=\(started ? "started" : "rejected")")
        } catch {
            log?("outcome=error")
        }
    }
}
#endif
