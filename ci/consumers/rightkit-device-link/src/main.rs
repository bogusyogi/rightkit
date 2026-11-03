// rightkit-device-link 0.1.0 consumed by exact registry pin (`=0.1.0`), outside the workspace.
// The crate is sans-IO, so the whole protocol runs in memory here: a SendQueue and a Receiver exchange
// encoded frames through a Vec. No socket, relay, file or process is used.
use rightkit_device_link::{
    decode, encode, DecodeError, EnqueueError, Frame, Limits, ReceiveError, Receiver, SendError,
    SendQueue, SkipReason, PROTOCOL_VERSION,
};

fn check(name: &str, ok: bool) {
    if !ok {
        eprintln!("FAIL: {name}");
        std::process::exit(1);
    }
    println!("ok: {name}");
}

// Move frames through the wire codec into the receiver.
fn pump(frames: Vec<Frame>, receiver: &mut Receiver, max_text: usize) {
    for frame in frames {
        let bytes = encode(&frame);
        let decoded = match decode(&bytes, max_text) {
            Ok(decoded) => decoded,
            Err(error) => {
                eprintln!("FAIL: decode of an encoded frame: {error}");
                std::process::exit(1);
            }
        };
        check("frame survives the wire", decoded == frame);
        if let Err(error) = receiver.on_frame(decoded) {
            eprintln!("FAIL: receiver refused an in-order frame: {error}");
            std::process::exit(1);
        }
    }
}

// Hand every ready message to a handler that accepts it, and return the acknowledgements.
fn drain(receiver: &mut Receiver, sender: &mut SendQueue) -> Vec<String> {
    let mut delivered = Vec::new();
    while let Some(delivery) = receiver.poll_delivery() {
        delivered.push(delivery.message.clone());
        let ack = receiver
            .accept(delivery.seq)
            .expect("accept the message out for handling");
        sender
            .on_frame(&ack)
            .expect("ack for a sequence the sender sent");
    }
    delivered
}

fn main() {
    check("protocol version", PROTOCOL_VERSION == 1);
    let defaults = Limits::default();
    check(
        "default limits",
        defaults.max_message_bytes == 4 * 1024 * 1024
            && defaults.fragment_bytes == 128 * 1024
            && defaults.max_fragments == 64
            && defaults.max_unacked == 64,
    );

    // Fragmenting, ordered delivery and cumulative acknowledgement. Four-byte fragments force a UTF-8
    // message to be split, and the split must not cut a character.
    let limits = Limits {
        fragment_bytes: 4,
        ..Limits::default()
    };
    let mut sender = SendQueue::new(7, limits);
    let mut receiver = Receiver::new(limits);
    check(
        "fresh queue",
        sender.stream_id() == 7 && sender.next_seq() == 0 && sender.unacked() == 0,
    );
    check(
        "first sequence",
        sender.enqueue("h\u{e9}llo w\u{f6}rld", 0) == Ok(0),
    );
    check("second sequence", sender.enqueue("second", 0) == Ok(1));
    check(
        "pending bytes counted",
        sender.pending_bytes() == 13 + 6 && sender.unacked() == 2,
    );
    let first_frames = sender.transmit();
    check(
        "a hello leads the first transmit",
        matches!(
            first_frames.first(),
            Some(Frame::Hello {
                stream_id: 7,
                base_seq: 0
            })
        ) && first_frames.len() > 3,
    );
    pump(first_frames, &mut receiver, limits.max_message_bytes);
    check(
        "nothing is accepted before the handler runs",
        receiver.next_expected() == 0,
    );
    let delivered = drain(&mut receiver, &mut sender);
    check(
        "messages arrive whole and in order",
        delivered == ["h\u{e9}llo w\u{f6}rld".to_string(), "second".to_string()],
    );
    check(
        "acknowledged messages leave the queue",
        sender.unacked() == 0 && sender.pending_bytes() == 0 && receiver.next_expected() == 2,
    );
    check(
        "reassembly buffers are empty",
        receiver.reassembly_bytes() == 0,
    );

    // A skip marker lets the stream advance past a message that failed permanently.
    check("third sequence", sender.enqueue("third", 10) == Ok(2));
    check("fourth sequence", sender.enqueue("fourth", 10) == Ok(3));
    check("skip a queued message", sender.skip(3, SkipReason::Failed));
    check(
        "a skipped message cannot be skipped again",
        !sender.skip(3, SkipReason::Failed),
    );
    pump(sender.transmit(), &mut receiver, limits.max_message_bytes);
    let after_skip = drain(&mut receiver, &mut sender);
    check(
        "the skipped message is never delivered",
        after_skip == ["third".to_string()]
            && receiver.next_expected() == 4
            && sender.unacked() == 0,
    );

    // Timeouts skip from the front.
    check("fifth sequence", sender.enqueue("fifth", 100) == Ok(4));
    check(
        "expire skips what waited too long",
        sender.expire(200, 50) == vec![4],
    );

    // Reconnecting replays under the original sequence numbers behind a new hello.
    sender.reconnect();
    let replay = sender.transmit();
    check(
        "replay starts with a hello at the base sequence",
        matches!(
            replay.first(),
            Some(Frame::Hello {
                stream_id: 7,
                base_seq: 4
            })
        ),
    );

    // Refusals.
    let mut stranger = Receiver::new(limits);
    check(
        "data before a hello is refused",
        stranger.on_frame(Frame::Data {
            stream_id: 7,
            seq: 0,
            index: 0,
            count: 1,
            text: "x".into(),
        }) == Err(ReceiveError::HelloRequired),
    );
    stranger
        .on_frame(Frame::Hello {
            stream_id: 7,
            base_seq: 0,
        })
        .expect("hello");
    check(
        "another stream is refused",
        stranger
            .on_frame(Frame::Ack {
                stream_id: 8,
                next_expected: 0,
            })
            .is_ok()
            && stranger.on_frame(Frame::Data {
                stream_id: 8,
                seq: 0,
                index: 0,
                count: 1,
                text: "x".into(),
            }) == Err(ReceiveError::StreamMismatch),
    );
    check(
        "accepting what was never delivered is refused",
        stranger.accept(0) == Err(ReceiveError::NotDelivered),
    );
    check(
        "ack beyond the last sent sequence is refused",
        SendQueue::new(1, limits).on_frame(&Frame::Ack {
            stream_id: 1,
            next_expected: 99,
        }) == Err(SendError::AckBeyondSent),
    );
    let small = Limits {
        max_message_bytes: 8,
        max_unacked: 1,
        ..Limits::default()
    };
    let mut bounded = SendQueue::new(2, small);
    check(
        "oversized message refused",
        bounded.enqueue("123456789", 0) == Err(EnqueueError::TooLarge),
    );
    check("first message fits", bounded.enqueue("1234", 0) == Ok(0));
    check(
        "window refuses the next message",
        bounded.enqueue("5", 0) == Err(EnqueueError::WindowFull),
    );

    // Wire decoding: plain payloads pass through, other versions fail closed.
    check(
        "plain bytes are not a frame",
        decode(b"plain text", 16) == Err(DecodeError::NotAFrame),
    );
    check(
        "other version fails closed",
        decode(b"RKDL\x02\x01", 16) == Err(DecodeError::UnsupportedVersion(2)),
    );
    check(
        "unknown frame type is malformed",
        decode(b"RKDL\x01\x09", 16) == Err(DecodeError::Malformed),
    );
    let ack = Frame::Ack {
        stream_id: 1,
        next_expected: 5,
    };
    let mut trailing = encode(&ack);
    trailing.push(0);
    check(
        "trailing bytes are malformed",
        decode(&trailing, 16) == Err(DecodeError::Malformed),
    );
    check("ack round trip", decode(&encode(&ack), 16) == Ok(ack));

    println!("OK rightkit-device-link 0.1.0 (registry consumer, public API)");
}
