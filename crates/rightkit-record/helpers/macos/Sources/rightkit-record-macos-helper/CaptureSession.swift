import AVFoundation
import CoreMedia
import CoreVideo
import Foundation
import RecordWire
import ScreenCaptureKit

/// One take: a window-scoped `SCStream` whose frame callback hands frames to a bounded queue; a
/// worker hashes each frame's raw pixels, appends it to an `AVAssetWriter` MP4 (`clean.mp4`), and
/// reports `ready` / `frame` / `dropped` events. The callback never hashes, encodes or writes.
///
/// Clock: sample PTS is the host clock, the same clock as `CLOCK_UPTIME_RAW` on Apple silicon
/// (RK2 Spike A measured 0.000 to 0.001 ms difference). `hostNs` is the sample PTS in ns. The
/// encoded PTS and `samplePtsNs` are host time minus the first accepted frame, so the core's clock
/// map is `hostNs - firstHostNs`, with a zero encoded-PTS offset.
final class CaptureSession: NSObject, SCStreamOutput, SCStreamDelegate {
    static let mediaFile = "clean.mp4"
    private static let queueCapacity = 3
    private static let monitorInterval: TimeInterval = 0.25

    private let plan: StartPlan
    private let window: SCWindow
    private let ownerPid: pid_t
    private let emit: (HelperEvent) -> Void
    private let width: Int
    private let height: Int
    private let mediaURL: URL
    private let writer: AVAssetWriter
    private let input: AVAssetWriterInput
    private var stream: SCStream?
    private let sampleQueue = DispatchQueue(label: "rightkit.record.samples", qos: .userInteractive)
    private let frames = FrameQueue(capacity: CaptureSession.queueCapacity)
    private let workerDone = DispatchSemaphore(value: 0)
    private let hasher = FrameHasher()
    private let frameIntervalMs: Double

    // Guarded by `lock`.
    private let lock = NSLock()
    private var stopping = false
    private var finished = false
    private var failureReported = false
    private var lostToBackpressure = 0
    private var lostUnknown = 0
    private var lastBounds: CGRect
    private var writerFailed = false

    // Worker-thread state.
    private var accepted: UInt64 = 0
    private var firstHostNs: UInt64?
    private var lastPtsNs: UInt64?
    private var lastHostNs: UInt64 = 0

    private init(plan: StartPlan, window: SCWindow, width: Int, height: Int, mediaURL: URL,
                 writer: AVAssetWriter, input: AVAssetWriterInput, emit: @escaping (HelperEvent) -> Void) {
        self.plan = plan
        self.window = window
        self.ownerPid = window.owningApplication?.processID ?? 0
        self.width = width
        self.height = height
        self.mediaURL = mediaURL
        self.writer = writer
        self.input = input
        self.emit = emit
        self.frameIntervalMs = 1000.0 / Double(plan.fps)
        self.lastBounds = window.frame
        super.init()
    }

    // MARK: start

    /// Validates the plan, refuses what the macOS backend cannot honor, then starts capture.
    /// Nothing is written to `plan.outDir` before permission is confirmed.
    static func start(plan: StartPlan, emit: @escaping (HelperEvent) -> Void) throws -> (CaptureSession, StartInfo) {
        guard (1...120).contains(plan.fps) else { throw HelperError.argInvalid("fps must be between 1 and 120, got \(plan.fps)") }
        guard plan.scale.isFinite, plan.scale > 0, plan.scale <= 4 else {
            throw HelperError.argInvalid("scale must be in (0, 4], got \(plan.scale)")
        }
        guard plan.audio.isEmpty else { throw HelperError.unsupported("the macOS backend captures no audio tracks yet") }
        switch plan.masking {
        case .unknown: throw HelperError.maskGeometryUnknown("pixel mask geometry is unknown; the take is refused before capture")
        case .declared(let regions) where !regions.isEmpty:
            throw HelperError.unsupported("the macOS backend does not apply declared pixel masks yet")
        default: break
        }
        switch plan.visibility {
        case .offScreenVisible, .onScreenVisible: break
        case .appHidden, .minimized, .occluded:
            throw HelperError.unsupported("visibility \(plan.visibility.rawValue) is not supported on macOS: a hidden or minimized window delivers no frames")
        }
        // Fail closed: no permission, nothing is created.
        guard screenAccessGranted() else {
            throw HelperError.permissionDenied("screen recording is not granted to this helper identity; nothing was written")
        }

        let window = try Windows.find(windowId: plan.windowId, pid: plan.pid)
        let observed = Windows.visibility(of: window)
        switch observed {
        case .appHidden: throw HelperError.windowHidden("the window's app is hidden; capture delivers no frames")
        case .minimized: throw HelperError.windowMinimized("the window is not on screen (minimized); capture delivers no frames")
        case .offScreenVisible where plan.visibility == .onScreenVisible, .onScreenVisible where plan.visibility == .offScreenVisible:
            throw HelperError.unsupported("the window is \(observed.rawValue) but the take declares \(plan.visibility.rawValue)")
        default: break
        }

        let width = evenPixels(Double(window.frame.width) * plan.scale)
        let height = evenPixels(Double(window.frame.height) * plan.scale)
        guard width >= 16, height >= 16, width <= 16384, height <= 16384 else {
            throw HelperError.argInvalid("the window would encode at \(width)x\(height)")
        }

        let outDir = URL(fileURLWithPath: plan.outDir, isDirectory: true)
        var isDirectory: ObjCBool = false
        guard FileManager.default.fileExists(atPath: outDir.path, isDirectory: &isDirectory), isDirectory.boolValue else {
            throw HelperError.io("output directory \(outDir.path) does not exist")
        }
        let mediaURL = outDir.appendingPathComponent(mediaFile)
        if FileManager.default.fileExists(atPath: mediaURL.path) {
            throw HelperError.outExists("\(mediaURL.path) already exists")
        }

        let (writer, input) = try makeWriter(url: mediaURL, codec: plan.codec, width: width, height: height, fps: Int(plan.fps))
        let session = CaptureSession(plan: plan, window: window, width: width, height: height,
                                     mediaURL: mediaURL, writer: writer, input: input, emit: emit)
        let hostNsAtStart = try session.beginCapture()
        let info = StartInfo(
            hostNsAtStart: hostNsAtStart, width: UInt32(width), height: UInt32(height), scale: plan.scale,
            boundsPt: Windows.rect(window.frame), dpi: UInt32((72.0 * Double(width) / Double(window.frame.width)).rounded()),
            codec: plan.codec.rawValue, osBuild: osBuild(), visibility: observed)
        return (session, info)
    }

    private static func evenPixels(_ value: Double) -> Int {
        let rounded = Int(value.rounded())
        return rounded + (rounded & 1)
    }

    private static func makeWriter(url: URL, codec: Codec, width: Int, height: Int, fps: Int) throws -> (AVAssetWriter, AVAssetWriterInput) {
        let bitrate = max(1_000_000, Int(Double(width * height) * Double(fps) * 0.15))
        let settings: [String: Any] = [
            AVVideoCodecKey: codec == .hevc ? AVVideoCodecType.hevc : AVVideoCodecType.h264,
            AVVideoWidthKey: width,
            AVVideoHeightKey: height,
            AVVideoCompressionPropertiesKey: [
                AVVideoAverageBitRateKey: bitrate,
                AVVideoExpectedSourceFrameRateKey: fps,
                AVVideoMaxKeyFrameIntervalKey: fps * 2,
                AVVideoAllowFrameReorderingKey: false,
            ] as [String: Any],
        ]
        let writer: AVAssetWriter
        do { writer = try AVAssetWriter(url: url, fileType: .mp4) } catch {
            throw HelperError.io("cannot create \(url.path): \(error.localizedDescription)")
        }
        guard writer.canApply(outputSettings: settings, forMediaType: .video) else {
            throw HelperError.codecUnavailable("\(codec.rawValue) at \(width)x\(height) is not available on this Mac")
        }
        let input = AVAssetWriterInput(mediaType: .video, outputSettings: settings)
        input.expectsMediaDataInRealTime = true
        guard writer.canAdd(input) else { throw HelperError.codecUnavailable("the writer refused the \(codec.rawValue) input") }
        writer.add(input)
        guard writer.startWriting() else {
            try? FileManager.default.removeItem(at: url)
            throw HelperError.codecUnavailable("the \(codec.rawValue) encoder could not start: \(writer.error?.localizedDescription ?? "unknown")")
        }
        return (writer, input)
    }

    /// Starts the stream and the worker. Returns the host time just before capture began.
    private func beginCapture() throws -> UInt64 {
        let filter = SCContentFilter(desktopIndependentWindow: window)
        let config = SCStreamConfiguration()
        config.width = width
        config.height = height
        config.minimumFrameInterval = CMTime(value: 1, timescale: CMTimeScale(plan.fps))
        config.queueDepth = 8
        config.pixelFormat = kCVPixelFormatType_32BGRA
        config.showsCursor = false
        config.capturesAudio = false
        config.ignoreShadowsSingleWindow = true
        config.captureResolution = .best
        let stream = SCStream(filter: filter, configuration: config, delegate: self)
        do {
            try stream.addStreamOutput(self, type: .screen, sampleHandlerQueue: sampleQueue)
        } catch {
            discardMedia()
            throw HelperError.helper("cannot attach the frame output: \(error.localizedDescription)")
        }
        self.stream = stream

        let worker = Thread { [self] in
            workerLoop()
            workerDone.signal()
        }
        worker.name = "rightkit.record.worker"
        worker.qualityOfService = .userInitiated
        worker.start()

        let semaphore = DispatchSemaphore(value: 0)
        var startError: Error?
        let hostNsAtStart = uptimeNs()
        stream.startCapture { error in
            startError = error
            semaphore.signal()
        }
        if semaphore.wait(timeout: .now() + 15) != .success || startError != nil {
            frames.close()
            _ = workerDone.wait(timeout: .now() + 5)
            discardMedia()
            let ns = startError as NSError?
            if let ns, ns.domain == SCStreamErrorDomain, ns.code == SCStreamError.userDeclined.rawValue {
                throw HelperError.permissionDenied("screen recording is not granted to this helper identity")
            }
            throw HelperError.helper("startCapture failed: \(ns?.localizedDescription ?? "timed out")")
        }
        let monitor = Thread { [self] in monitorLoop() }
        monitor.name = "rightkit.record.monitor"
        monitor.start()
        return hostNsAtStart
    }

    private func discardMedia() {
        writer.cancelWriting()
        try? FileManager.default.removeItem(at: mediaURL)
    }

    // MARK: SCStreamOutput

    func stream(_ stream: SCStream, didOutputSampleBuffer sampleBuffer: CMSampleBuffer, of type: SCStreamOutputType) {
        guard type == .screen, sampleBuffer.isValid else { return }
        guard let attachments = CMSampleBufferGetSampleAttachmentsArray(sampleBuffer, createIfNecessary: false) as? [[SCStreamFrameInfo: Any]],
              let raw = attachments.first?[.status] as? Int, let status = SCFrameStatus(rawValue: raw) else { return }
        // Only `complete` frames carry new pixels. `idle` means the content did not change, which is
        // silence, not a drop. `suspended` (hidden or minimized target) is caught by the monitor.
        guard status == .complete, let pixels = CMSampleBufferGetImageBuffer(sampleBuffer) else { return }
        if CVPixelBufferGetWidth(pixels) != width || CVPixelBufferGetHeight(pixels) != height {
            lock.lock(); lostUnknown += 1; lock.unlock()
            return
        }
        let pts = CMTimeConvertScale(CMSampleBufferGetPresentationTimeStamp(sampleBuffer), timescale: 1_000_000_000, method: .default)
        guard pts.isValid, pts.value >= 0 else { return }
        if !frames.push(QueuedFrame(sample: sampleBuffer, hostNs: UInt64(pts.value))) {
            lock.lock(); lostToBackpressure += 1; lock.unlock()
        }
    }

    func stream(_ stream: SCStream, didStopWithError error: Error) {
        lock.lock()
        let expected = stopping
        lock.unlock()
        if expected { return }
        switch probeWindow(window.windowID) {
        case .gone: fail(.targetExited)
        case .alive: fail(.io("the capture stream stopped: \(error.localizedDescription)"))
        }
    }

    // MARK: worker

    private func workerLoop() {
        while let frame = frames.pop() {
            process(frame)
        }
        flushLosses(atHostNs: lastHostNs)
    }

    private func takeLosses() -> (backpressure: Int, unknown: Int) {
        lock.lock()
        defer { lock.unlock() }
        let taken = (lostToBackpressure, lostUnknown)
        lostToBackpressure = 0
        lostUnknown = 0
        return taken
    }

    private func flushLosses(atHostNs hostNs: UInt64) {
        let lost = takeLosses()
        report(drop: lost.backpressure, reason: .backpressure, atHostNs: hostNs)
        report(drop: lost.unknown, reason: .unknown, atHostNs: hostNs)
    }

    private func report(drop count: Int, reason: DropReason, atHostNs hostNs: UInt64) {
        guard count > 0 else { return }
        emit(.dropped(hostNs: hostNs, count: UInt32(count), intervalMs: Double(count) * frameIntervalMs, reason: reason))
    }

    private func process(_ frame: QueuedFrame) {
        lastHostNs = frame.hostNs
        // Frames lost in the callback since the last accepted frame are reported before this one.
        flushLosses(atHostNs: frame.hostNs)

        lock.lock()
        let broken = writerFailed
        lock.unlock()
        if broken { return }

        let first = firstHostNs ?? frame.hostNs
        let pts = frame.hostNs >= first ? frame.hostNs - first : 0
        if let last = lastPtsNs, pts <= last || frame.hostNs < first {
            report(drop: 1, reason: .unknown, atHostNs: frame.hostNs)
            return
        }
        guard writer.status == .writing else {
            writerBroke()
            return
        }
        guard input.isReadyForMoreMediaData, let pixels = CMSampleBufferGetImageBuffer(frame.sample) else {
            report(drop: 1, reason: .encoder, atHostNs: frame.hostNs)
            return
        }

        CVPixelBufferLockBaseAddress(pixels, .readOnly)
        guard let base = CVPixelBufferGetBaseAddress(pixels) else {
            CVPixelBufferUnlockBaseAddress(pixels, .readOnly)
            report(drop: 1, reason: .unknown, atHostNs: frame.hostNs)
            return
        }
        let rawHash = hasher.hashPackedBGRA(
            base: base, width: CVPixelBufferGetWidth(pixels), height: CVPixelBufferGetHeight(pixels),
            bytesPerRow: CVPixelBufferGetBytesPerRow(pixels))
        CVPixelBufferUnlockBaseAddress(pixels, .readOnly)

        if firstHostNs == nil {
            writer.startSession(atSourceTime: .zero)
            firstHostNs = frame.hostNs
        }
        var timing = CMSampleTimingInfo(
            duration: CMSampleBufferGetDuration(frame.sample),
            presentationTimeStamp: CMTime(value: CMTimeValue(pts), timescale: 1_000_000_000),
            decodeTimeStamp: .invalid)
        var retimed: CMSampleBuffer?
        CMSampleBufferCreateCopyWithNewTiming(
            allocator: kCFAllocatorDefault, sampleBuffer: frame.sample, sampleTimingEntryCount: 1,
            sampleTimingArray: &timing, sampleBufferOut: &retimed)
        guard let retimed, input.append(retimed) else {
            writerBroke()
            return
        }
        let sample = FrameSample(index: accepted, hostNs: frame.hostNs, samplePtsNs: pts, rawHash: rawHash)
        emit(accepted == 0 ? .ready(sample) : .frame(sample))
        accepted += 1
        lastPtsNs = pts
    }

    private func writerBroke() {
        lock.lock()
        writerFailed = true
        lock.unlock()
        let error = writer.error as NSError?
        let diskFull = error.map { isDiskFull($0) } ?? false
        fail(diskFull ? .diskFull : .io("the encoder failed: \(error?.localizedDescription ?? "unknown")"))
    }

    private func isDiskFull(_ error: NSError) -> Bool {
        if error.code == -11807 || (error.domain == NSPOSIXErrorDomain && error.code == Int(ENOSPC)) { return true }
        if let underlying = error.userInfo[NSUnderlyingErrorKey] as? NSError { return isDiskFull(underlying) }
        return false
    }

    // MARK: monitor

    /// Watches the target while the take runs: it must stay alive, on screen, in the same place,
    /// and the permission must stay granted. One failure is reported, then the monitor idles.
    private func monitorLoop() {
        while true {
            Thread.sleep(forTimeInterval: CaptureSession.monitorInterval)
            lock.lock()
            let done = stopping || finished
            let reported = failureReported
            lock.unlock()
            if done { return }
            if reported { continue }
            if !screenAccessGranted() { fail(.permissionRevoked); continue }
            if ownerPid != 0, kill(ownerPid, 0) == -1, errno == ESRCH { fail(.targetExited); continue }
            switch probeWindow(window.windowID) {
            case .gone:
                fail(.targetExited)
            case .alive(let onScreen, let bounds):
                if !onScreen {
                    fail(notOnScreenFailure(pid: ownerPid))
                } else if moved(to: bounds) {
                    emit(.geometry(hostNs: uptimeNs(), boundsPt: Windows.rect(bounds), scale: plan.scale,
                                   dpi: UInt32((72.0 * Double(width) / max(Double(bounds.width), 1)).rounded())))
                }
            }
        }
    }

    private func moved(to bounds: CGRect) -> Bool {
        lock.lock()
        defer { lock.unlock() }
        let delta = abs(bounds.minX - lastBounds.minX) + abs(bounds.minY - lastBounds.minY)
            + abs(bounds.width - lastBounds.width) + abs(bounds.height - lastBounds.height)
        if delta < 0.5 || bounds.width <= 0 || bounds.height <= 0 { return false }
        lastBounds = bounds
        return true
    }

    private func fail(_ failure: Failure) {
        lock.lock()
        let first = !failureReported
        failureReported = true
        lock.unlock()
        if first { emit(.failed(failure)) }
    }

    // MARK: finish

    /// Stops capture, drains the queue, and finalizes the MP4. `clean` is false when the media
    /// could not be finalized. A take with no accepted frame leaves no media file: the core ends
    /// it `invalid` (no accepted frames).
    func finish() -> Finished {
        lock.lock()
        if finished { lock.unlock(); return Finished(clean: !writerFailed) }
        stopping = true
        lock.unlock()

        if let stream {
            let semaphore = DispatchSemaphore(value: 0)
            stream.stopCapture { _ in semaphore.signal() }
            _ = semaphore.wait(timeout: .now() + 10)
        }
        sampleQueue.sync {}  // no callback is running after this returns
        frames.close()
        let drained = workerDone.wait(timeout: .now() + 30) == .success

        lock.lock()
        finished = true
        let broken = writerFailed
        lock.unlock()

        if accepted == 0 {
            discardMedia()
            return Finished(clean: drained)
        }
        if writer.status == .writing {
            input.markAsFinished()
            let semaphore = DispatchSemaphore(value: 0)
            writer.finishWriting { semaphore.signal() }
            if semaphore.wait(timeout: .now() + 60) != .success {
                writer.cancelWriting()
                return Finished(clean: false)
            }
        }
        return Finished(clean: drained && !broken && writer.status == .completed)
    }
}
