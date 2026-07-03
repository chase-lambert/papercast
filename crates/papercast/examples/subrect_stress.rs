//! M0b throwaway stress: prove that two *disjoint* `update_cropped` calls per
//! tick reach a client as two separate small rects (not one merged bounding
//! box). This is the mechanism M3's tile-based dirty updates rely on.
//!
//! Run: `cargo run -p papercast --example subrect_stress`
//! then check with a client on 127.0.0.1:5901.

use std::sync::Arc;
use std::time::Duration;

use rustvncserver::VncServer;

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let (server, _events) = VncServer::new(640, 480, "subrect-stress".into(), None);
    let server = Arc::new(server);
    {
        let server = Arc::clone(&server);
        tokio::spawn(async move {
            server.listen("127.0.0.1:5901").await.expect("listen failed");
        });
    }

    let mut shade: u8 = 0;
    loop {
        tokio::time::sleep(Duration::from_millis(100)).await;
        shade = shade.wrapping_add(16);
        let tile = vec![shade; 64 * 64 * 4];
        let fb = server.framebuffer();
        // Far corners: these must never intersect, so they must stay two rects.
        fb.update_cropped(&tile, 0, 0, 64, 64).await.unwrap();
        fb.update_cropped(&tile, 576, 416, 64, 64).await.unwrap();
    }
}
