// rk2-capture: RK2 Spike A helper (macOS). Adapted from ScreenRight's ScreenRightDriver.swift (single-window
// SCContentFilter + SCRecordingOutput path) and ClockAnchors.swift (host/media clock anchors), but with
//  - onScreenWindowsOnly:false window discovery by (pid, windowID), never "first window of the bundle",
//  - an SCStreamOutput frame-callback path (needed for host-clock -> sample-PTS -> encoded-PTS mapping),
//  - an SCRecordingOutput direct-to-file path kept only to demonstrate it has no per-frame callback,
//  - a Vision/pixel analyzer for the decision table.
// Subcommands: list | record | analyze. Spike code, not production.
import AppKit
import AVFoundation
import CoreMedia
import CoreVideo
import ScreenCaptureKit
import Vision

func uptimeNs() -> UInt64 { clock_gettime_nsec_np(CLOCK_UPTIME_RAW) }
func jsonLine(_ d: [String: Any]) -> String {
    // NSJSONSerialization raises an ObjC exception (abort + crash dialog) on NaN/inf: refuse invalid objects instead.
    guard JSONSerialization.isValidJSONObject(d), let data = try? JSONSerialization.data(withJSONObject: d, options: [.sortedKeys]), let s = String(data: data, encoding: .utf8) else { return "{}" }
    return s
}
func emit(_ d: [String: Any]) { print(jsonLine(d)); fflush(stdout) }
func fail(_ code: String, _ msg: String) -> Never {
    emit(["ok": false, "error": code, "message": msg]); exit(code == "permission_denied" ? 3 : 1)
}
func args(_ a: [String]) -> [String: String] {
    var r: [String: String] = [:]; var i = 0
    while i < a.count { if a[i].hasPrefix("--"), i + 1 < a.count { r[String(a[i].dropFirst(2))] = a[i + 1]; i += 2 } else { i += 1 } }
    return r
}

// R13: fail closed. CGPreflightScreenCaptureAccess never prompts; nothing is written when it is false.
func requireScreenAccess() {
    if !CGPreflightScreenCaptureAccess() { fail("permission_denied", "screen recording not granted to this helper identity; nothing written") }
}

func shareableWindows() -> [SCWindow] {
    let sem = DispatchSemaphore(value: 0); var out: [SCWindow] = []; var err: Error?
    SCShareableContent.getExcludingDesktopWindows(false, onScreenWindowsOnly: false) { c, e in out = c?.windows ?? []; err = e; sem.signal() }
    if sem.wait(timeout: .now() + 10) != .success { fail("sck_timeout", "content discovery timed out") }
    if let err { fail("sck_error", (err as NSError).localizedDescription) }
    return out
}

// MARK: record

final class FileDelegate: NSObject, SCRecordingOutputDelegate {
    let started = DispatchSemaphore(value: 0), finished = DispatchSemaphore(value: 0)
    var failure: Error?
    func recordingOutputDidStartRecording(_ r: SCRecordingOutput) { started.signal() }
    func recordingOutput(_ r: SCRecordingOutput, didFailWithError e: any Error) { failure = e; started.signal(); finished.signal() }
    func recordingOutputDidFinishRecording(_ r: SCRecordingOutput) { finished.signal() }
}

final class StreamSink: NSObject, SCStreamOutput {
    let writer: AVAssetWriter, input: AVAssetWriterInput
    let framesLog: FileHandle
    var firstPTS: CMTime?
    var n = 0, appended = 0, complete = 0, idle = 0, blank = 0, other = 0
    var firstArrival: UInt64 = 0
    var lastPTS: CMTime = .zero
    let lock = NSLock()
    var maxPTSDeltaNs: Double = 0, minPTSDeltaNs: Double = .infinity
    init(url: URL, width: Int, height: Int, fps: Int, framesLog: FileHandle) throws {
        writer = try AVAssetWriter(url: url, fileType: .mp4)
        input = AVAssetWriterInput(mediaType: .video, outputSettings: [
            AVVideoCodecKey: AVVideoCodecType.h264, AVVideoWidthKey: width, AVVideoHeightKey: height,
            AVVideoCompressionPropertiesKey: [AVVideoAverageBitRateKey: 8_000_000, AVVideoExpectedSourceFrameRateKey: fps, AVVideoMaxKeyFrameIntervalKey: fps * 2],
        ])
        input.expectsMediaDataInRealTime = true
        writer.add(input); self.framesLog = framesLog
        super.init()
        writer.startWriting()
    }
    func stream(_ stream: SCStream, didOutputSampleBuffer sb: CMSampleBuffer, of type: SCStreamOutputType) {
        guard type == .screen, sb.isValid else { return }
        let arrival = uptimeNs()
        lock.lock(); defer { lock.unlock() }
        var statusName = "none"
        var contentRect: [String: Double]? = nil
        if let arr = CMSampleBufferGetSampleAttachmentsArray(sb, createIfNecessary: false) as? [[SCStreamFrameInfo: Any]], let a = arr.first {
            if let raw = a[.status] as? Int, let st = SCFrameStatus(rawValue: raw) {
                switch st { case .complete: statusName = "complete"; complete += 1; case .idle: statusName = "idle"; idle += 1
                case .blank: statusName = "blank"; blank += 1; case .suspended: statusName = "suspended"; other += 1
                case .started: statusName = "started"; other += 1; case .stopped: statusName = "stopped"; other += 1; @unknown default: statusName = "unknown"; other += 1 }
            }
            if let cr = a[.contentRect] as? [String: Any] ?? nil, let w = cr["Width"] as? Double, let h = cr["Height"] as? Double { contentRect = ["w": w, "h": h] }
        }
        let pts = CMSampleBufferGetPresentationTimeStamp(sb)
        n += 1
        let ptsS = pts.seconds
        let deltaNs = Double(arrival) - ptsS * 1e9   // arrival (uptime) minus sample PTS (host clock): callback latency
        if statusName == "complete" { maxPTSDeltaNs = max(maxPTSDeltaNs, deltaNs); minPTSDeltaNs = min(minPTSDeltaNs, deltaNs) }
        var enc: Double? = nil
        var didAppend = false
        if statusName == "complete" {
            if firstPTS == nil {
                firstPTS = pts; firstArrival = arrival
                writer.startSession(atSourceTime: .zero)
                emit(["event": "first_accepted_frame", "arrival_uptime_ns": arrival, "pts_host_s": ptsS, "latency_ms": deltaNs / 1e6])
            }
            var timing = CMSampleTimingInfo(duration: CMSampleBufferGetDuration(sb), presentationTimeStamp: CMTimeSubtract(pts, firstPTS!), decodeTimeStamp: .invalid)
            var copy: CMSampleBuffer?
            CMSampleBufferCreateCopyWithNewTiming(allocator: kCFAllocatorDefault, sampleBuffer: sb, sampleTimingEntryCount: 1, sampleTimingArray: &timing, sampleBufferOut: &copy)
            if let copy, input.isReadyForMoreMediaData, writer.status == .writing { didAppend = input.append(copy); if didAppend { appended += 1 } }
            enc = CMTimeSubtract(pts, firstPTS!).seconds
        }
        var rec: [String: Any] = ["i": n, "arrival_uptime_ns": arrival, "pts_host_s": ptsS, "status": statusName, "appended": didAppend]
        if let enc { rec["pts_encoded_s"] = enc }
        if let contentRect { rec["content"] = contentRect }
        if let pb = CMSampleBufferGetImageBuffer(sb) { rec["w"] = CVPixelBufferGetWidth(pb); rec["h"] = CVPixelBufferGetHeight(pb) }
        if let line = (jsonLine(rec) + "\n").data(using: .utf8) { framesLog.write(line) }
        lastPTS = pts
    }
}

func frontmostSnapshot(excludingPids: Set<Int32>) -> [String: Any] {
    let f = NSWorkspace.shared.frontmostApplication
    var top: [String: Any] = [:]
    if let list = CGWindowListCopyWindowInfo([.optionOnScreenOnly, .excludeDesktopElements], kCGNullWindowID) as? [[String: Any]] {
        for w in list where (w[kCGWindowLayer as String] as? Int) == 0 {
            let pid = Int32((w[kCGWindowOwnerPID as String] as? Int) ?? 0)
            if excludingPids.contains(pid) { continue }
            top = ["pid": pid, "owner": w[kCGWindowOwnerName as String] as? String ?? "", "windowNumber": w[kCGWindowNumber as String] as? Int ?? 0]; break
        }
    }
    return ["frontmost_bundle": f?.bundleIdentifier ?? "?", "frontmost_pid": Int(f?.processIdentifier ?? 0), "top_other_window": top]
}

func record(_ a: [String: String]) {
    requireScreenAccess()
    guard let wid = a["window-id"].flatMap({ UInt32($0) }), let outDir = a["out"] else { fail("usage", "record --window-id N --out DIR [--fps 30] [--seconds 60] [--scale 2] [--mode stream|file] [--exclude-pids a,b]") }
    let fps = Int(a["fps"] ?? "30") ?? 30, seconds = Double(a["seconds"] ?? "60") ?? 60
    let mode = a["mode"] ?? "stream"
    let excl = Set((a["exclude-pids"] ?? "").split(separator: ",").compactMap { Int32($0) })
    try? FileManager.default.createDirectory(atPath: outDir, withIntermediateDirectories: true)
    let wins = shareableWindows()
    guard let win = wins.first(where: { $0.windowID == wid }) else {
        emit(["ok": false, "error": "window_not_found", "windowID": wid, "sck_window_count": wins.count]); exit(2)
    }
    let scale = Double(a["scale"] ?? "") ?? 2
    let width = Int((win.frame.width * scale).rounded()), height = Int((win.frame.height * scale).rounded())
    let filter = SCContentFilter(desktopIndependentWindow: win)
    let cfg = SCStreamConfiguration()
    cfg.width = width; cfg.height = height
    cfg.minimumFrameInterval = CMTime(value: 1, timescale: CMTimeScale(fps))
    cfg.queueDepth = 6; cfg.pixelFormat = kCVPixelFormatType_32BGRA
    cfg.showsCursor = false; cfg.capturesAudio = false
    cfg.ignoreShadowsSingleWindow = true
    cfg.captureResolution = .best
    let stream = SCStream(filter: filter, configuration: cfg, delegate: nil)
    let mp4 = URL(fileURLWithPath: outDir).appendingPathComponent("capture.mp4")
    try? FileManager.default.removeItem(at: mp4)
    let framesURL = URL(fileURLWithPath: outDir).appendingPathComponent("frames.jsonl")
    FileManager.default.createFile(atPath: framesURL.path, contents: nil)
    let framesLog = try! FileHandle(forWritingTo: framesURL)
    var sink: StreamSink?
    var fileDelegate: FileDelegate?
    let q = DispatchQueue(label: "rk2.frames", qos: .userInteractive)
    if mode == "stream" {
        do { sink = try StreamSink(url: mp4, width: width, height: height, fps: fps, framesLog: framesLog); try stream.addStreamOutput(sink!, type: .screen, sampleHandlerQueue: q) }
        catch { fail("setup_failed", "\(error)") }
    } else {
        let rc = SCRecordingOutputConfiguration(); rc.outputURL = mp4; rc.videoCodecType = .h264; rc.outputFileType = .mp4
        fileDelegate = FileDelegate()
        let ro = SCRecordingOutput(configuration: rc, delegate: fileDelegate!)
        do { try stream.addRecordingOutput(ro) } catch { fail("setup_failed", "\(error)") }
    }
    // Clock anchors (ClockAnchors.swift pattern): uptime ns vs CMClock host time sampled back to back.
    let anchor0: [String: Any] = ["uptime_ns": uptimeNs(), "host_clock_s": CMClockGetTime(CMClockGetHostTimeClock()).seconds]
    let sem = DispatchSemaphore(value: 0); var startErr: Error?
    let startCall = uptimeNs()
    stream.startCapture { e in startErr = e; sem.signal() }
    if sem.wait(timeout: .now() + 15) != .success { fail("start_timeout", "startCapture timed out") }
    if let startErr { fail("start_failed", (startErr as NSError).localizedDescription) }
    emit(["event": "capture_started", "mode": mode, "fps": fps, "width": width, "height": height, "scale": scale, "windowID": wid,
          "sck_frame": ["x": win.frame.minX, "y": win.frame.minY, "w": win.frame.width, "h": win.frame.height],
          "sck_isOnScreen": win.isOnScreen, "sck_layer": win.windowLayer, "sck_active": win.isActive,
          "start_call_uptime_ns": startCall, "start_return_uptime_ns": uptimeNs(), "anchor0": anchor0])
    if let fd = fileDelegate { _ = fd.started.wait(timeout: .now() + 10) }
    // Foreground-stability monitor (R1): record frontmost app and topmost non-fixture window on change.
    var fgLog: [[String: Any]] = []
    let fg0 = frontmostSnapshot(excludingPids: excl)
    fgLog.append(["t_uptime_ns": uptimeNs(), "snapshot": fg0, "kind": "before"])
    var lastKey = jsonLine(fg0)
    let t0 = Date()
    var nextAnchor = 1.0
    var anchors: [[String: Any]] = [["kind": "start", "uptime_ns": anchor0["uptime_ns"]!, "host_clock_s": anchor0["host_clock_s"]!]]
    while Date().timeIntervalSince(t0) < seconds {
        let s = frontmostSnapshot(excludingPids: excl); let k = jsonLine(s)
        if k != lastKey { fgLog.append(["t_uptime_ns": uptimeNs(), "snapshot": s, "kind": "change"]); lastKey = k }
        let el = Date().timeIntervalSince(t0)
        if el >= nextAnchor { anchors.append(["kind": "checkpoint", "uptime_ns": uptimeNs(), "host_clock_s": CMClockGetTime(CMClockGetHostTimeClock()).seconds]); nextAnchor += 1 }
        Thread.sleep(forTimeInterval: 0.05)
    }
    let stopSem = DispatchSemaphore(value: 0)
    let stopCall = uptimeNs()
    stream.stopCapture { _ in stopSem.signal() }
    _ = stopSem.wait(timeout: .now() + 15)
    var summary: [String: Any] = ["event": "capture_done", "mode": mode, "stop_call_uptime_ns": stopCall]
    if let sink {
        q.sync {}  // drain
        sink.input.markAsFinished()
        let fs = DispatchSemaphore(value: 0); sink.writer.finishWriting { fs.signal() }; _ = fs.wait(timeout: .now() + 30)
        summary["writer_status"] = sink.writer.status.rawValue; summary["writer_error"] = sink.writer.error.map { "\($0)" } ?? ""
        summary["callbacks"] = sink.n; summary["appended"] = sink.appended; summary["complete"] = sink.complete; summary["idle"] = sink.idle
        summary["blank"] = sink.blank; summary["other_status"] = sink.other
        summary["callback_latency_ms"] = sink.complete > 0 ? ["min": sink.minPTSDeltaNs / 1e6, "max": sink.maxPTSDeltaNs / 1e6] : ["min": -1.0, "max": -1.0]
        summary["first_pts_host_s"] = sink.firstPTS?.seconds ?? -1; summary["first_arrival_uptime_ns"] = sink.firstArrival
    } else if let fd = fileDelegate {
        _ = fd.finished.wait(timeout: .now() + 15)
        summary["file_failure"] = fd.failure.map { "\($0)" } ?? ""
        summary["per_frame_callbacks"] = "none (SCRecordingOutput exposes only started/finished/failed)"
    }
    anchors.append(["kind": "stop", "uptime_ns": uptimeNs(), "host_clock_s": CMClockGetTime(CMClockGetHostTimeClock()).seconds])
    let fgAfter = frontmostSnapshot(excludingPids: excl)
    fgLog.append(["t_uptime_ns": uptimeNs(), "snapshot": fgAfter, "kind": "after"])
    summary["foreground_events"] = fgLog; summary["clock_anchors"] = anchors
    summary["file_bytes"] = (try? FileManager.default.attributesOfItem(atPath: mp4.path)[.size] as? Int) ?? -1
    try? framesLog.close()
    try? jsonLine(summary).write(to: URL(fileURLWithPath: outDir).appendingPathComponent("summary.json"), atomically: true, encoding: .utf8)
    emit(["event": "summary_written", "path": outDir + "/summary.json"])
    exit(0)
}

// MARK: analyze

func analyze(_ a: [String: String]) {
    guard let video = a["video"], let out = a["out"] else { fail("usage", "analyze --video f.mp4 --out analysis.jsonl --scale 2 --content-top 28 [--ocr-ms 500]") }
    let scale = Double(a["scale"] ?? "2") ?? 2, top = Double(a["content-top"] ?? "28") ?? 28
    let ocrMs = Double(a["ocr-ms"] ?? "500") ?? 500
    let asset = AVURLAsset(url: URL(fileURLWithPath: video))
    guard let track = asset.tracks(withMediaType: .video).first, let reader = try? AVAssetReader(asset: asset) else { fail("bad_video", "cannot open video") }
    let outp = AVAssetReaderTrackOutput(track: track, outputSettings: [kCVPixelBufferPixelFormatTypeKey as String: kCVPixelFormatType_32BGRA])
    reader.add(outp); reader.startReading()
    FileManager.default.createFile(atPath: out, contents: nil)
    let fh = try! FileHandle(forWritingTo: URL(fileURLWithPath: out))
    var lastOCR = -1e9
    var idx = 0
    // --ocr-windows "a:b,c:d" (encoded seconds): OCR every frame inside these windows, every --ocr-ms elsewhere.
    let wins: [(Double, Double)] = (a["ocr-windows"] ?? "").split(separator: ",").compactMap { p in
        let q = p.split(separator: ":"); guard q.count == 2, let x = Double(q[0]), let y = Double(q[1]) else { return nil }; return (x, y) }
    let bits = 24
    while let sb = outp.copyNextSampleBuffer() {
        guard let pb = CMSampleBufferGetImageBuffer(sb) else { continue }
        let pts = CMSampleBufferGetPresentationTimeStamp(sb).seconds
        CVPixelBufferLockBaseAddress(pb, .readOnly)
        let w = CVPixelBufferGetWidth(pb), h = CVPixelBufferGetHeight(pb), bpr = CVPixelBufferGetBytesPerRow(pb)
        let base = CVPixelBufferGetBaseAddress(pb)!.assumingMemoryBound(to: UInt8.self)
        func px(_ x: Int, _ y: Int) -> (r: Int, g: Int, b: Int) {
            let xx = min(max(x, 0), w - 1), yy = min(max(y, 0), h - 1); let p = base + yy * bpr + xx * 4
            return (Int(p[2]), Int(p[1]), Int(p[0]))
        }
        func luma(_ x: Int, _ y: Int) -> Int { let c = px(x, y); return (c.r * 3 + c.g * 6 + c.b) / 10 }
        // whole-frame stats on a 16px grid
        var sum = 0, cnt = 0, magenta = 0
        var yy = 0
        while yy < h { var xx = 0; while xx < w { let c = px(xx, yy); sum += (c.r * 3 + c.g * 6 + c.b) / 10; cnt += 1
            if c.r > 200 && c.b > 200 && c.g < 90 { magenta += 1 }; xx += 16 }; yy += 16 }
        let meanLuma = Double(sum) / Double(max(cnt, 1))
        // strip: guard dark at cell 0, guard bright at cell 25; cell i at x=(8+8*i+4)pt, y=(top+8+4)pt
        func cellLuma(_ i: Int) -> Int { luma(Int((8.0 + 8.0 * Double(i) + 4.0) * scale), Int((top + 12.0) * scale)) }
        let g0 = cellLuma(0), g1 = cellLuma(bits + 1)
        var counter = 0
        for i in 0..<bits { counter = (counter << 1) | (cellLuma(i + 1) > 128 ? 1 : 0) }
        let valid = g0 < 100 && g1 > 160
        var rec: [String: Any] = ["i": idx, "pts": pts, "mean_luma": meanLuma, "magenta": magenta, "strip_valid": valid, "counter": valid ? counter : -1, "w": w, "h": h]
        if (pts * 1000 - lastOCR) >= ocrMs || wins.contains(where: { pts >= $0.0 && pts <= $0.1 }) {
            lastOCR = pts * 1000
            // OCR crop: step id + frame lines at top-left of the content area
            // OCR region of interest: step id + frame lines at the top-left of the content area (Vision ROI is normalized, bottom-left origin)
            let cy = (top + 20.0) * scale, ch = 88.0 * scale, cw = 520.0 * scale
            let req = VNRecognizeTextRequest(); req.recognitionLevel = .accurate; req.usesLanguageCorrection = false
            req.regionOfInterest = CGRect(x: 0, y: max(0, 1.0 - (cy + ch) / Double(h)), width: min(1, cw / Double(w)), height: min(1, ch / Double(h)))
            do { try VNImageRequestHandler(cvPixelBuffer: pb, options: [:]).perform([req]) } catch { rec["ocr_error"] = "\(error)" }
            rec["ocr"] = (req.results ?? []).compactMap { $0.topCandidates(1).first?.string }
        }
        CVPixelBufferUnlockBaseAddress(pb, .readOnly)
        fh.write((jsonLine(rec) + "\n").data(using: .utf8)!)
        idx += 1
    }
    try? fh.close()
    emit(["ok": true, "frames": idx, "reader_status": reader.status.rawValue])
}

func zorder(_ a: [String: String]) {
    let pids = Set((a["pids"] ?? "").split(separator: ",").compactMap { Int($0) })
    let l = (CGWindowListCopyWindowInfo([.optionOnScreenOnly], kCGNullWindowID) as? [[String: Any]]) ?? []  // on-screen list is reliably front-to-back; .optionAll order is not
    var out: [[String: Any]] = []
    for (i, w) in l.enumerated() {
        let pid = (w[kCGWindowOwnerPID as String] as? Int) ?? 0
        if !pids.contains(pid) { continue }
        let b = w[kCGWindowBounds as String] as? [String: Any] ?? [:]
        out.append(["front_to_back_index": i, "pid": pid, "windowNumber": w[kCGWindowNumber as String] as? Int ?? 0, "layer": w[kCGWindowLayer as String] as? Int ?? 0,
                    "onscreen": w[kCGWindowIsOnscreen as String] as? Bool ?? false, "bounds": ["x": b["X"] ?? 0, "y": b["Y"] ?? 0, "w": b["Width"] ?? 0, "h": b["Height"] ?? 0]])
    }
    emit(["ok": true, "windows": out])
}

func list(_ a: [String: String]) {
    requireScreenAccess()
    let pid = a["pid"].flatMap { Int32($0) }
    let ws = shareableWindows().filter { pid == nil || $0.owningApplication?.processID == pid }
    emit(["ok": true, "windows": ws.map { ["windowID": $0.windowID, "title": $0.title ?? "", "pid": Int($0.owningApplication?.processID ?? 0),
        "isOnScreen": $0.isOnScreen, "layer": $0.windowLayer, "active": $0.isActive,
        "frame": ["x": $0.frame.minX, "y": $0.frame.minY, "w": $0.frame.width, "h": $0.frame.height]] as [String: Any] }])
}

// Spike helper on a shared desktop: fatal signals exit silently (no ReportCrash dialog).
for sig in [SIGSEGV, SIGBUS, SIGABRT, SIGILL, SIGTRAP] { signal(sig) { s in _exit(128 + s) } }
let argv = Array(CommandLine.arguments.dropFirst())
guard let cmd = argv.first else { fail("usage", "list|record|analyze") }
let opts = args(Array(argv.dropFirst()))
_ = NSApplication.shared  // connects to the window server for NSWorkspace; never activated, no UI
Thread.detachNewThread {
    switch cmd { case "list": list(opts); exit(0); case "zorder": zorder(opts); exit(0); case "record": record(opts); case "analyze": analyze(opts); exit(0); default: fail("usage", "unknown \(cmd)") }
}
RunLoop.main.run()
