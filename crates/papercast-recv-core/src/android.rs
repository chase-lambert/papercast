//! JNI bindings for the Kotlin shell (M11b). Gated behind the `android` feature
//! so the host build and tests never pull `jni`. Nothing here runs on the host —
//! it is compile-checked with `--features android` and cross-compiled with
//! cargo-ndk; its first real execution is the M11b emulator run.
//!
//! # Java contract
//!
//! The Kotlin side declares (package `com.papercast`, class `RecvCore`):
//!
//! ```kotlin
//! class RecvCore {
//!     external fun nativeStart(addr: String, callback: FrameCallback): Long
//!     external fun nativeStop(handle: Long)
//!     companion object { init { System.loadLibrary("papercast_recv_core") } }
//! }
//!
//! interface FrameCallback {
//!     fun onConnect(width: Int, height: Int, levels: Int)
//!     fun onFrame(pixels: java.nio.ByteBuffer, width: Int, height: Int, refreshHint: Int)
//!     fun onModeChanged(name: String)
//!     fun onDisconnect()
//! }
//! ```
//!
//! `onFrame`'s `pixels` is a **direct** `ByteBuffer` over the receiver's reused
//! Gray8 framebuffer (`width * height` bytes, row-major). It is valid only for
//! the duration of the call — copy it into your `Bitmap` and return promptly; do
//! not retain the buffer. Returning is also the back-pressure signal: the core
//! sends its next `Ready` only after `onFrame` returns, so a slow draw throttles
//! the sender.
//!
//! Lifecycle notes for the shell:
//! - `nativeStop` may block up to ~3 s if a connection attempt is in flight (the
//!   connect timeout is not interrupted); call it off the UI thread.
//! - `onDisconnect` also fires as part of a clean `nativeStop`, not only on an
//!   error — treat it as "connection ended", not "failure".
//! - `refreshHint`: 0 = Auto, 1 = Fast, 2 = Quality. Map it to a device waveform
//!   in your `RefreshBackend`; the core never does.

use jni::objects::{GlobalRef, JClass, JObject, JString, JValue};
use jni::sys::jlong;
use jni::{JNIEnv, JavaVM};

use crate::{FrameSink, FrameView, Receiver};

/// A [`FrameSink`] that forwards each callback to a Java `FrameCallback`.
struct JniSink {
    vm: JavaVM,
    callback: GlobalRef,
}

impl JniSink {
    /// A `JNIEnv` for the current (receiver) thread. Idempotent: returns the
    /// existing env if the thread is already attached, otherwise attaches it as a
    /// daemon (so it never blocks JVM shutdown). All callbacks run on the one
    /// receiver thread, so this attaches once and reuses thereafter.
    fn env(&self) -> jni::errors::Result<JNIEnv<'_>> {
        self.vm.attach_current_thread_as_daemon()
    }

    /// Run one Java callback inside a fresh JNI local frame, so every local
    /// reference it creates (the direct `ByteBuffer`, a `String`) is released
    /// when the frame pops. This is mandatory here: the receiver thread is
    /// natively attached and never returns to Java, so without a per-call frame
    /// those refs accumulate until ART's local-reference table overflows and
    /// aborts the process — within seconds at streaming rates.
    fn invoke<F>(&self, f: F)
    where
        F: FnOnce(&mut JNIEnv, &GlobalRef) -> jni::errors::Result<()>,
    {
        let Ok(mut env) = self.env() else { return };
        if env.with_local_frame(4, |env| f(env, &self.callback)).is_err() {
            // A callback that threw leaves an exception pending on the thread;
            // clear it so the next call's JNI operations start clean.
            let _ = env.exception_clear();
        }
    }
}

impl FrameSink for JniSink {
    fn on_frame(&mut self, frame: FrameView<'_>) {
        let (w, h) = (i32::from(frame.width), i32::from(frame.height));
        let hint = i32::from(frame.refresh_hint.to_u8());
        let ptr = frame.pixels.as_ptr().cast_mut();
        let len = frame.pixels.len();
        self.invoke(|env, callback| {
            // A direct ByteBuffer over the reused framebuffer — no pixel copy.
            // The memory outlives this synchronous call; Java must not retain it.
            let buf = unsafe { env.new_direct_byte_buffer(ptr, len)? };
            env.call_method(
                callback,
                "onFrame",
                "(Ljava/nio/ByteBuffer;III)V",
                &[JValue::Object(&buf), JValue::Int(w), JValue::Int(h), JValue::Int(hint)],
            )?;
            Ok(())
        });
    }

    fn on_connect(&mut self, width: u16, height: u16, levels: u8) {
        let (w, h, l) = (i32::from(width), i32::from(height), i32::from(levels));
        self.invoke(|env, callback| {
            env.call_method(
                callback,
                "onConnect",
                "(III)V",
                &[JValue::Int(w), JValue::Int(h), JValue::Int(l)],
            )?;
            Ok(())
        });
    }

    fn on_mode_changed(&mut self, name: &str) {
        self.invoke(|env, callback| {
            let jname = env.new_string(name)?;
            env.call_method(
                callback,
                "onModeChanged",
                "(Ljava/lang/String;)V",
                &[JValue::Object(&jname)],
            )?;
            Ok(())
        });
    }

    fn on_disconnect(&mut self) {
        self.invoke(|env, callback| {
            env.call_method(callback, "onDisconnect", "()V", &[])?;
            Ok(())
        });
    }
}

/// `RecvCore.nativeStart(addr, callback)` → an opaque handle (0 on failure).
///
/// # Safety
/// Called only by the JVM through the declared `external` method; the returned
/// handle must be passed to exactly one [`Java_com_papercast_RecvCore_nativeStop`].
#[no_mangle]
pub extern "system" fn Java_com_papercast_RecvCore_nativeStart<'local>(
    mut env: JNIEnv<'local>,
    _this: JObject<'local>,
    addr: JString<'local>,
    callback: JObject<'local>,
) -> jlong {
    let Ok(addr) = env.get_string(&addr) else { return 0 };
    let addr: String = addr.into();
    let Ok(vm) = env.get_java_vm() else { return 0 };
    let Ok(callback) = env.new_global_ref(callback) else { return 0 };
    match crate::start(&addr, JniSink { vm, callback }) {
        Ok(recv) => Box::into_raw(Box::new(recv)) as jlong,
        Err(_) => 0,
    }
}

/// `RecvCore.nativeStop(handle)` — stop the receiver and free it. A 0 handle is a
/// no-op, so double-stop is safe as long as the shell nulls its handle.
///
/// # Safety
/// `handle` must be a value returned by [`Java_com_papercast_RecvCore_nativeStart`]
/// and not previously passed here.
#[no_mangle]
pub extern "system" fn Java_com_papercast_RecvCore_nativeStop(
    _env: JNIEnv,
    _class: JClass,
    handle: jlong,
) {
    if handle == 0 {
        return;
    }
    // Reclaim the Box leaked in nativeStart; its Drop signals stop and joins the
    // worker thread.
    drop(unsafe { Box::from_raw(handle as *mut Receiver) });
}
