import AppKit
import CoreGraphics
import CoreMedia
import CoreVideo
import Foundation
import RecordWire
import ScreenCaptureKit

// Whole-screen capture (rightkit#443): `rightkit-record-macos-helper capture-screen` records one
// display, or every display composited, with ScreenCaptureKit into the lossless `screen.rkv` that
// crates/rightkit-record/src/rkv.rs documents and reads, and `rightkit-record-macos-helper screens`
// lists the displays. Unlike the window helper this mode does not speak the length-prefixed protocol:
// stdout carries one JSON object per line and stdin takes `stop` (see src/macos/screen.rs).
//
// Clock: sample PTS is the host clock, the same clock as CLOCK_UPTIME_RAW on Apple silicon (the window
// capture measured 0.000 to 0.001 ms difference), so `hostNs` in the file is the sample PTS in
// nanoseconds and the Rust side stamps markers with `clock_gettime_nsec_np(CLOCK_UPTIME_RAW)`.
//
// Not built or run: written without compiling; the next gate run on a Mac is the first compile.

// MARK: output

enum ScreenOut {
    private static let lock = NSLock()

    static func emit(_ object: [String: Any]) {
        guard let data = try? JSONSerialization.data(withJSONObject: object, options: [.sortedKeys]) else { return }
        lock.lock()
        defer { lock.unlock() }
        FileHandle.standardOutput.write(data)
        FileHandle.standardOutput.write(Data("\n".utf8))
    }

    static func failed(_ error: HelperError) {
        emit(["event": "failed", "code": error.code, "message": error.message])
    }
}

// MARK: displays

struct DisplayPlan {
    let display: SCDisplay
    let index: Int
    let pixelWidth: Int
    let pixelHeight: Int
    /// Top left corner on the canvas, in pixels (filled in once the canvas is known).
    var x: Int
    var y: Int
    let primary: Bool
    /// Top left corner in global capture pixels (points times the display's pixel scale).
    let globalX: Int
    let globalY: Int

    var json: [String: Any] {
        ["index": index, "x": globalX, "y": globalY, "width": pixelWidth, "height": pixelHeight, "primary": primary]
    }
}

enum Displays {
    static func shareable() throws -> [SCDisplay] {
        let semaphore = DispatchSemaphore(value: 0)
        var displays: [SCDisplay] = []
        var failure: Error?
        SCShareableContent.getExcludingDesktopWindows(false, onScreenWindowsOnly: false) { content, error in
            displays = content?.displays ?? []
            failure = error
            semaphore.signal()
        }
        if semaphore.wait(timeout: .now() + 10) != .success {
            throw HelperError.helper("display discovery timed out")
        }
        if let failure {
            let ns = failure as NSError
            if ns.domain == SCStreamErrorDomain && ns.code == SCStreamError.userDeclined.rawValue {
                throw HelperError.permissionDenied("screen recording is not granted to this helper identity")
            }
            throw HelperError.helper("display discovery failed: \(ns.localizedDescription)")
        }
        return displays
    }

    /// Native pixel size of a display.
    static func pixelSize(_ display: SCDisplay) -> (width: Int, height: Int) {
        if let mode = CGDisplayCopyDisplayMode(display.displayID) {
            return (Int(mode.pixelWidth), Int(mode.pixelHeight))
        }
        return (Int(display.width), Int(display.height))
    }

    /// The displays in capture order: the main display first, then left to right, top to bottom.
    static func plans() throws -> [DisplayPlan] {
        guard screenAccessGranted() else {
            throw HelperError.permissionDenied("screen recording is not granted to this helper identity; nothing was written")
        }
        let main = CGMainDisplayID()
        let sorted = try shareable().sorted { a, b in
            let aMain = a.displayID == main, bMain = b.displayID == main
            if aMain != bMain { return aMain }
            if a.frame.minX != b.frame.minX { return a.frame.minX < b.frame.minX }
            return a.frame.minY < b.frame.minY
        }
        return sorted.enumerated().map { index, display in
            let size = pixelSize(display)
            let scale = display.frame.width > 0 ? Double(size.width) / Double(display.frame.width) : 1
            return DisplayPlan(
                display: display, index: index, pixelWidth: size.width, pixelHeight: size.height, x: 0, y: 0,
                primary: display.displayID == main,
                globalX: Int((Double(display.frame.minX) * scale).rounded()),
                globalY: Int((Double(display.frame.minY) * scale).rounded()))
        }
    }
}

// MARK: RKV1 writer

/// Writes the `RKV1` container (crates/rightkit-record/src/rkv.rs): a 16 byte header, then one record per
/// frame, each frame XORed with the previous one and run-length coded. Every frame is written with one
/// write, so a killed capture keeps every whole frame before it.
final class RKVWriter {
    private static let minZeroRun = 8
    private let handle: FileHandle
    private var previous: [UInt8]
    private var payload: [UInt8] = []
    private(set) var frames: UInt64 = 0
    private(set) var failed = false

    init(url: URL, width: Int, height: Int) throws {
        guard FileManager.default.createFile(atPath: url.path, contents: nil, attributes: nil) else {
            throw HelperError.io("cannot create \(url.path)")
        }
        handle = try FileHandle(forWritingTo: url)
        previous = [UInt8](repeating: 0, count: width * height * 4)
        var header = Data("RKV1".utf8)
        header.append(RKVWriter.le32(UInt32(width)))
        header.append(RKVWriter.le32(UInt32(height)))
        header.append(RKVWriter.le32(1))  // pixel format 1: packed BGRA, 8 bits
        try handle.write(contentsOf: header)
    }

    private static func le32(_ value: UInt32) -> Data {
        withUnsafeBytes(of: value.littleEndian) { Data($0) }
    }

    private static func le64(_ value: UInt64) -> Data {
        withUnsafeBytes(of: value.littleEndian) { Data($0) }
    }

    private static func pushVarint(_ out: inout [UInt8], _ value: Int) {
        var rest = value
        while true {
            let low = UInt8(rest & 0x7F)
            rest >>= 7
            if rest == 0 {
                out.append(low)
                return
            }
            out.append(low | 0x80)
        }
    }

    /// Appends one frame taken at `hostNs`. `bgra` is `width * height * 4` packed bytes.
    func write(hostNs: UInt64, bgra: [UInt8]) {
        guard !failed, bgra.count == previous.count else { failed = true; return }
        payload.removeAll(keepingCapacity: true)
        let count = bgra.count
        previous.withUnsafeBufferPointer { before in
            bgra.withUnsafeBufferPointer { now in
                var at = 0
                while at < count {
                    var runEnd = at
                    while runEnd < count && now[runEnd] == before[runEnd] { runEnd += 1 }
                    if runEnd > at && (runEnd - at >= RKVWriter.minZeroRun || runEnd == count) {
                        payload.append(0)
                        RKVWriter.pushVarint(&payload, runEnd - at)
                        at = runEnd
                        continue
                    }
                    // A literal runs until the next unchanged run long enough to be its own token, or the end.
                    let start = at
                    var end = at
                    while true {
                        while end < count && now[end] != before[end] { end += 1 }
                        if end == count { break }
                        var zerosEnd = end
                        while zerosEnd < count && now[zerosEnd] == before[zerosEnd] { zerosEnd += 1 }
                        if zerosEnd - end >= RKVWriter.minZeroRun || zerosEnd == count { break }
                        end = zerosEnd
                    }
                    payload.append(1)
                    RKVWriter.pushVarint(&payload, end - start)
                    for i in start..<end { payload.append(now[i] ^ before[i]) }
                    at = end
                }
            }
        }
        var record = RKVWriter.le64(hostNs)
        record.append(RKVWriter.le32(UInt32(payload.count)))
        record.append(RKVWriter.le32(0))
        record.append(contentsOf: payload)
        do {
            try handle.write(contentsOf: record)
        } catch {
            failed = true
            return
        }
        previous = bgra
        frames += 1
    }

    /// Syncs and closes the file. False when a write failed at any point.
    func finish() -> Bool {
        let synced = (try? handle.synchronize()) != nil
        try? handle.close()
        return synced && !failed
    }
}

// MARK: session

/// One take: a display-scoped `SCStream` per captured display, whose callbacks hand frames to a bounded
/// queue; a worker copies each frame onto the canvas at its display's place, keeps frames on the fps
/// grid, and appends the canvas to `screen.rkv`. The callback never copies, encodes or writes.
final class ScreenSession: NSObject, SCStreamOutput, SCStreamDelegate {
    private let plans: [DisplayPlan]
    private let canvasWidth: Int
    private let canvasHeight: Int
    private let fps: Int
    private let writer: RKVWriter
    private var streams: [SCStream] = []
    private var indexByStream: [ObjectIdentifier: Int] = [:]
    private let sampleQueue = DispatchQueue(label: "rightkit.record.screen.samples", qos: .userInteractive)
    private let frames = FrameQueue(capacity: 6)
    private let workerDone = DispatchSemaphore(value: 0)
    private let intervalNs: Int64

    private let lock = NSLock()
    private var stopping = false
    private var finished = false
    private var failureReported = false
    private var lostToBackpressure = 0
    private var lostUnknown = 0

    // Worker-thread state.
    private var canvas: [UInt8]
    private var nextDueNs: Int64?
    private var lastNs: UInt64 = 0
    private var kept: UInt64 = 0
    private var dropped: UInt64 = 0

    private init(plans: [DisplayPlan], canvasWidth: Int, canvasHeight: Int, fps: Int, writer: RKVWriter) {
        self.plans = plans
        self.canvasWidth = canvasWidth
        self.canvasHeight = canvasHeight
        self.fps = fps
        self.writer = writer
        self.intervalNs = Int64(1_000_000_000) / Int64(fps)
        self.canvas = [UInt8](repeating: 0, count: canvasWidth * canvasHeight * 4)
        super.init()
    }

    /// Validates, creates `screen.rkv`, and starts every stream. Nothing is written before permission
    /// is confirmed.
    static func start(target: String, fps: Int, outDir: String) throws -> ScreenSession {
        guard (1...120).contains(fps) else { throw HelperError.argInvalid("fps must be between 1 and 120, got \(fps)") }
        var chosen = try Displays.plans()
        if chosen.isEmpty { throw HelperError.notFound("no display is attached") }
        if target != "virtual" {
            let text = target.hasPrefix("monitor:") ? String(target.dropFirst("monitor:".count)) : target
            guard let wanted = Int(text) else { throw HelperError.argInvalid("--target must be virtual or monitor:N, got \(target)") }
            guard let plan = chosen.first(where: { $0.index == wanted }) else {
                throw HelperError.notFound("no monitor \(wanted); \(chosen.count) are attached")
            }
            chosen = [plan]
        }
        // The canvas is the union of the chosen displays, each at its place relative to the union's corner.
        let left = chosen.map { $0.globalX }.min() ?? 0
        let top = chosen.map { $0.globalY }.min() ?? 0
        let right = chosen.map { $0.globalX + $0.pixelWidth }.max() ?? 0
        let bottom = chosen.map { $0.globalY + $0.pixelHeight }.max() ?? 0
        for i in chosen.indices {
            chosen[i].x = chosen[i].globalX - left
            chosen[i].y = chosen[i].globalY - top
        }
        let width = right - left
        let height = bottom - top
        guard width > 0, height > 0, width <= 16384, height <= 16384 else {
            throw HelperError.unsupported("the \(width)x\(height) canvas is outside 1...16384 pixels a side")
        }

        let outURL = URL(fileURLWithPath: outDir, isDirectory: true)
        var isDirectory: ObjCBool = false
        guard FileManager.default.fileExists(atPath: outURL.path, isDirectory: &isDirectory), isDirectory.boolValue else {
            throw HelperError.io("output directory \(outURL.path) does not exist")
        }
        let videoURL = outURL.appendingPathComponent("screen.rkv")
        if FileManager.default.fileExists(atPath: videoURL.path) {
            throw HelperError.outExists("\(videoURL.path) already exists")
        }
        let writer = try RKVWriter(url: videoURL, width: width, height: height)
        let session = ScreenSession(plans: chosen, canvasWidth: width, canvasHeight: height, fps: fps, writer: writer)
        do {
            try session.begin()
        } catch {
            session.discard(videoURL)
            throw error
        }
        return session
    }

    private func discard(_ url: URL) {
        frames.close()
        _ = writer.finish()
        try? FileManager.default.removeItem(at: url)
    }

    /// The monitors that make up the canvas, for the `started` event.
    var monitorsJSON: [[String: Any]] { plans.map { $0.json } }
    var size: (width: Int, height: Int) { (canvasWidth, canvasHeight) }

    private func begin() throws {
        let worker = Thread { [self] in
            workerLoop()
            workerDone.signal()
        }
        worker.name = "rightkit.record.screen.worker"
        worker.qualityOfService = .userInitiated
        worker.start()

        for plan in plans {
            let filter = SCContentFilter(display: plan.display, excludingWindows: [])
            let config = SCStreamConfiguration()
            config.width = plan.pixelWidth
            config.height = plan.pixelHeight
            config.minimumFrameInterval = CMTime(value: 1, timescale: CMTimeScale(fps))
            config.queueDepth = 8
            config.pixelFormat = kCVPixelFormatType_32BGRA
            config.showsCursor = false
            config.capturesAudio = false
            config.captureResolution = .best
            let stream = SCStream(filter: filter, configuration: config, delegate: self)
            do {
                try stream.addStreamOutput(self, type: .screen, sampleHandlerQueue: sampleQueue)
            } catch {
                stopStreams()
                throw HelperError.helper("cannot attach the frame output: \(error.localizedDescription)")
            }
            streams.append(stream)
            indexByStream[ObjectIdentifier(stream)] = plan.index
        }
        for stream in streams {
            let semaphore = DispatchSemaphore(value: 0)
            var startError: Error?
            stream.startCapture { error in
                startError = error
                semaphore.signal()
            }
            if semaphore.wait(timeout: .now() + 15) != .success || startError != nil {
                let ns = startError as NSError?
                stopStreams()
                if let ns, ns.domain == SCStreamErrorDomain, ns.code == SCStreamError.userDeclined.rawValue {
                    throw HelperError.permissionDenied("screen recording is not granted to this helper identity")
                }
                throw HelperError.helper("startCapture failed: \(ns?.localizedDescription ?? "timed out")")
            }
        }
        let monitor = Thread { [self] in monitorLoop() }
        monitor.name = "rightkit.record.screen.monitor"
        monitor.start()
    }

    private func stopStreams() {
        for stream in streams {
            let semaphore = DispatchSemaphore(value: 0)
            stream.stopCapture { _ in semaphore.signal() }
            _ = semaphore.wait(timeout: .now() + 10)
        }
        sampleQueue.sync {}  // no callback is running after this returns
        frames.close()
        _ = workerDone.wait(timeout: .now() + 30)
    }

    // MARK: SCStreamOutput

    func stream(_ stream: SCStream, didOutputSampleBuffer sampleBuffer: CMSampleBuffer, of type: SCStreamOutputType) {
        guard type == .screen, sampleBuffer.isValid else { return }
        guard let attachments = CMSampleBufferGetSampleAttachmentsArray(sampleBuffer, createIfNecessary: false) as? [[SCStreamFrameInfo: Any]],
              let raw = attachments.first?[.status] as? Int, let status = SCFrameStatus(rawValue: raw) else { return }
        // Only `complete` frames carry new pixels; `idle` means the content did not change, which is silence, not a drop.
        guard status == .complete, let pixels = CMSampleBufferGetImageBuffer(sampleBuffer),
              let display = indexByStream[ObjectIdentifier(stream)], display < plans.count else { return }
        let plan = plans[display]
        if CVPixelBufferGetWidth(pixels) != plan.pixelWidth || CVPixelBufferGetHeight(pixels) != plan.pixelHeight {
            lock.lock(); lostUnknown += 1; lock.unlock()
            return
        }
        let pts = CMTimeConvertScale(CMSampleBufferGetPresentationTimeStamp(sampleBuffer), timescale: 1_000_000_000, method: .default)
        guard pts.isValid, pts.value >= 0 else { return }
        if !frames.push(QueuedFrame(sample: sampleBuffer, hostNs: UInt64(pts.value), display: display)) {
            lock.lock(); lostToBackpressure += 1; lock.unlock()
        }
    }

    func stream(_ stream: SCStream, didStopWithError error: Error) {
        lock.lock()
        let expected = stopping
        lock.unlock()
        if expected { return }
        fail(HelperError.io("the capture stream stopped: \(error.localizedDescription)"))
    }

    // MARK: worker

    private func workerLoop() {
        while let frame = frames.pop() {
            process(frame)
        }
        flushLosses(atHostNs: lastNs)
    }

    private func flushLosses(atHostNs hostNs: UInt64) {
        lock.lock()
        let lost = lostToBackpressure + lostUnknown
        lostToBackpressure = 0
        lostUnknown = 0
        lock.unlock()
        guard lost > 0 else { return }
        dropped += UInt64(lost)
        ScreenOut.emit(["event": "dropped", "hostNs": hostNs, "count": lost])
    }

    private func process(_ frame: QueuedFrame) {
        flushLosses(atHostNs: frame.hostNs)
        guard frame.display < plans.count, let pixels = CMSampleBufferGetImageBuffer(frame.sample) else { return }
        let plan = plans[frame.display]

        // Every arrival updates the canvas, kept or not, so a skipped frame is never lost content.
        CVPixelBufferLockBaseAddress(pixels, .readOnly)
        guard let base = CVPixelBufferGetBaseAddress(pixels) else {
            CVPixelBufferUnlockBaseAddress(pixels, .readOnly)
            lock.lock(); lostUnknown += 1; lock.unlock()
            return
        }
        let bytesPerRow = CVPixelBufferGetBytesPerRow(pixels)
        let rowBytes = plan.pixelWidth * 4
        canvas.withUnsafeMutableBytes { destination in
            guard let target = destination.baseAddress else { return }
            for row in 0..<plan.pixelHeight {
                let source = base.advanced(by: row * bytesPerRow)
                let offset = ((plan.y + row) * canvasWidth + plan.x) * 4
                memcpy(target + offset, source, rowBytes)
            }
        }
        CVPixelBufferUnlockBaseAddress(pixels, .readOnly)

        // Timestamps in the file never go back, even when two displays' callbacks race.
        let hostNs = max(frame.hostNs, lastNs)
        if !keep(Int64(hostNs)) { return }
        writer.write(hostNs: hostNs, bgra: canvas)
        if writer.failed {
            fail(HelperError.io("writing screen.rkv failed"))
            return
        }
        lastNs = hostNs
        kept += 1
    }

    /// The fps grid, as the Windows pacer does it: a frame is kept when it lands up to a third of an interval early.
    private func keep(_ hostNs: Int64) -> Bool {
        let tolerance = intervalNs / 3
        guard let due = nextDueNs else {
            nextDueNs = hostNs + intervalNs
            return true
        }
        if hostNs + tolerance >= due {
            nextDueNs = max(due + intervalNs, hostNs + intervalNs - tolerance)
            return true
        }
        return false
    }

    // MARK: monitor

    /// Permission must stay granted while the take runs. One failure is reported, then the monitor idles.
    private func monitorLoop() {
        while true {
            Thread.sleep(forTimeInterval: 0.5)
            lock.lock()
            let done = stopping || finished
            lock.unlock()
            if done { return }
            if !screenAccessGranted() { fail(HelperError("permission_revoked", "screen recording was withdrawn mid-take")) }
        }
    }

    private func fail(_ error: HelperError) {
        lock.lock()
        let first = !failureReported
        failureReported = true
        lock.unlock()
        if first { ScreenOut.failed(error) }
    }

    // MARK: finish

    /// Stops capture, drains the queue and closes the file. Emits `finished`. `clean` is false when a
    /// write failed or the take failed mid-way.
    func finish() {
        lock.lock()
        if finished { lock.unlock(); return }
        stopping = true
        lock.unlock()
        stopStreams()
        lock.lock()
        finished = true
        let reported = failureReported
        lock.unlock()
        let intact = writer.finish()
        ScreenOut.emit(["event": "finished", "clean": intact && !reported, "frames": writer.frames, "dropped": dropped])
    }
}

// MARK: command

enum ScreenCommand {
    /// `screens` or `capture-screen ...`. Never returns: the process exits when the take ends.
    static func run(_ arguments: [String]) -> Never {
        do {
            switch arguments.first {
            case "screens": try listScreens()
            case "capture-screen": try capture(Array(arguments.dropFirst()))
            default: throw HelperError.argInvalid("unknown screen command")
            }
        } catch let error as HelperError {
            ScreenOut.failed(error)
            exit(1)
        } catch {
            ScreenOut.failed(HelperError.helper("\(error)"))
            exit(1)
        }
        exit(0)
    }

    private static func listScreens() throws {
        let plans = try Displays.plans()
        ScreenOut.emit(["event": "screens", "monitors": plans.map { $0.json }])
    }

    private static func capture(_ arguments: [String]) throws {
        var out: String?
        var fps: Int?
        var target = "virtual"
        var iterator = arguments.makeIterator()
        while let flag = iterator.next() {
            guard let value = iterator.next() else { throw HelperError.argInvalid("\(flag) needs a value") }
            switch flag {
            case "--out": out = value
            case "--fps": fps = Int(value)
            case "--target": target = value
            default: throw HelperError.argInvalid("unknown flag \(flag)")
            }
        }
        guard let out, !out.isEmpty else { throw HelperError.argInvalid("--out is required") }
        guard let fps else { throw HelperError.argInvalid("--fps is required and must be a whole number") }

        let hostNsAtStart = uptimeNs()
        let session = try ScreenSession.start(target: target, fps: fps, outDir: out)
        ScreenOut.emit([
            "event": "started", "hostNsAtStart": hostNsAtStart, "width": session.size.width, "height": session.size.height,
            "osBuild": osBuild(), "monitors": session.monitorsJSON,
        ])

        // SIGTERM (rightkit-child-process termination): finalize the video, then exit.
        signal(SIGTERM, SIG_IGN)
        let terminate = DispatchSource.makeSignalSource(signal: SIGTERM, queue: .global())
        terminate.setEventHandler {
            session.finish()
            exit(0)
        }
        terminate.resume()

        // `stop` on stdin, or stdin closing, finalizes the take.
        Thread.detachNewThread {
            while let line = readLine() {
                if line.trimmingCharacters(in: .whitespaces) == "stop" { break }
            }
            session.finish()
            exit(0)
        }
        RunLoop.main.run()
        exit(0)
    }
}
