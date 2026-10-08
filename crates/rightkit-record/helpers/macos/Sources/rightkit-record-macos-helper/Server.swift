import Foundation
import RecordWire

/// Serves the helper protocol: one request at a time on stdin, responses and events on stdout.
/// Events are written by the capture worker as they happen; every event of a take is written
/// before the response to `stop` or `abort`, because `finish()` joins the worker first.
final class Server {
    private let output = FrameWriter(fd: 1)
    private var session: CaptureSession?
    private let sessionLock = NSLock()

    func send(_ object: [String: Any]) {
        do { try output.write(object) } catch {
            // The core is gone. Finalize what is on disk and leave.
            log("cannot write to the core (\(error)); ending")
            abortAndExit(code: 1)
        }
    }

    func run() -> Never {
        let reader = FrameReader(fd: 0)
        while true {
            let body: Data?
            do { body = try reader.next() } catch {
                log("request stream broke: \(error)")
                abortAndExit(code: 1)
            }
            // The core closed its side without stop: end the take, nothing more is answered.
            guard let body else { abortAndExit(code: 0) }
            let request: Request
            do { request = try Request.decode(try Framing.parse(body)) } catch {
                log("unreadable request: \(error)")
                abortAndExit(code: 1)
            }
            handle(request)
        }
    }

    private func handle(_ request: Request) {
        switch request.op {
        case "hello":
            send(Messages.response(id: request.id, result: Messages.identity(
                osBuild: osBuild(), helperIdentity: helperIdentity, signed: helperIsSigned(),
                permission: screenAccessGranted() ? .granted : .denied)))
        case "sources":
            do {
                guard screenAccessGranted() else { throw HelperError.permissionDenied("screen recording is not granted to this helper identity") }
                send(Messages.response(id: request.id, sources: try Windows.sources()))
            } catch { reply(request.id, error) }
        case "start":
            start(request)
        case "stop":
            sessionLock.lock()
            let current = session
            session = nil
            sessionLock.unlock()
            guard let current else {
                send(Messages.errorResponse(id: request.id, .notRecording("the helper has no take to stop")))
                finish(code: 0)
            }
            let finished = current.finish()
            send(Messages.response(id: request.id, result: Messages.finished(finished)))
            finish(code: 0)
        case "abort":
            sessionLock.lock()
            let current = session
            session = nil
            sessionLock.unlock()
            _ = current?.finish()
            send(Messages.response(id: request.id, result: [:]))
            finish(code: 0)
        default:
            send(Messages.errorResponse(id: request.id, .argInvalid("unknown operation \(request.op)")))
        }
    }

    private func start(_ request: Request) {
        sessionLock.lock()
        let busy = session != nil
        sessionLock.unlock()
        if busy {
            send(Messages.errorResponse(id: request.id, .alreadyRecording("the helper already has a take")))
            return
        }
        do {
            guard let raw = request.plan else { throw HelperError.argInvalid("missing field 'plan'") }
            let plan: StartPlan
            do { plan = try StartPlan.decode(raw) } catch { throw HelperError.argInvalid("\(error)") }
            let (started, info) = try CaptureSession.start(plan: plan) { [weak self] event in
                self?.send(Messages.event(event))
            }
            sessionLock.lock()
            session = started
            sessionLock.unlock()
            send(Messages.response(id: request.id, result: Messages.startInfo(info)))
        } catch {
            reply(request.id, error)
        }
    }

    private func reply(_ id: UInt64, _ error: Error) {
        let refusal = (error as? HelperError) ?? HelperError.helper("\(error)")
        send(Messages.errorResponse(id: id, refusal))
    }

    /// bye, then exit: nothing follows.
    private func finish(code: Int32) -> Never {
        send(Messages.bye)
        exit(code)
    }

    /// Ends any take, finalizing the media if possible, then exits. Also the SIGTERM path.
    func abortAndExit(code: Int32) -> Never {
        sessionLock.lock()
        let current = session
        session = nil
        sessionLock.unlock()
        _ = current?.finish()
        exit(code)
    }
}
