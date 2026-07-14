//! Reconnect-with-backoff, exercised against a tiny controllable server so the
//! test can drop a connection deterministically. The real end-to-end sender
//! path is exercised by the native transport tests in the `papercast` crate.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use papercast_proto::{
    decode, encode, Message, Rect, RefreshHint, ServerHello, Update, UpdateRect, PROTO_VERSION,
};
use papercast_recv_core::{start, FrameSink, FrameView};

/// Collects delivered frames without gating the stream (returns immediately).
struct CollectSink {
    out: mpsc::Sender<u8>,
}

impl FrameSink for CollectSink {
    fn on_frame(&mut self, frame: FrameView<'_>) {
        // Every pixel is the same fill in this test; report the top-left one.
        let _ = self.out.send(frame.pixels[0]);
    }
}

/// Read from `stream` until a `Ready` message is seen (the client sends
/// `ClientHello` then `Ready`), or the peer closes.
fn read_until_ready(stream: &mut TcpStream) {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 256];
    loop {
        loop {
            match decode(&buf) {
                Ok(Some((msg, consumed))) => {
                    buf.drain(..consumed);
                    if msg == Message::Ready {
                        return;
                    }
                }
                Ok(None) => break,
                Err(_) => return,
            }
        }
        match stream.read(&mut chunk) {
            Ok(0) | Err(_) => return,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
        }
    }
}

/// Send a `ServerHello` + one full-frame `Update` filled with `fill`.
fn greet_and_send(stream: &mut TcpStream, fill: u8) {
    stream
        .write_all(&encode(&Message::ServerHello(ServerHello {
            proto_version: PROTO_VERSION,
            width: 2,
            height: 2,
            levels: 16,
        })))
        .unwrap();
    read_until_ready(stream);
    stream
        .write_all(&encode(&Message::Update(Update {
            refresh_hint: RefreshHint::Quality,
            rects: vec![UpdateRect {
                rect: Rect { x: 0, y: 0, width: 2, height: 2 },
                gray8: vec![fill; 4],
            }],
        })))
        .unwrap();
}

#[test]
fn reconnects_after_the_link_drops() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();

    // Server: first connection sends a 100-fill frame then hangs up; the second
    // connection (after the receiver reconnects) sends a 200-fill frame.
    let server = thread::spawn(move || {
        let (mut c1, _) = listener.accept().unwrap();
        greet_and_send(&mut c1, 100);
        drop(c1); // force the receiver to notice EOF and reconnect

        let (mut c2, _) = listener.accept().unwrap();
        greet_and_send(&mut c2, 200);
        // Hold the connection open long enough for delivery before teardown.
        thread::sleep(Duration::from_millis(300));
    });

    let (out_tx, out_rx) = mpsc::channel();
    let recv = start(&addr.to_string(), CollectSink { out: out_tx }).unwrap();

    let first = out_rx.recv_timeout(Duration::from_secs(5)).expect("first frame");
    assert_eq!(first, 100, "first connection's frame");
    let second = out_rx.recv_timeout(Duration::from_secs(5)).expect("frame after reconnect");
    assert_eq!(second, 200, "second connection's frame proves reconnect worked");

    recv.stop();
    server.join().unwrap();
}
