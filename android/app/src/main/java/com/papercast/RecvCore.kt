package com.papercast

import java.nio.ByteBuffer

/**
 * Callbacks from the native receiver core. All fire on the core's own receiver
 * thread (never the UI thread), so implementations must be thread-safe with
 * respect to whatever they touch.
 *
 * The Rust side of this contract lives in `papercast-recv-core/src/android.rs`;
 * the JNI method signatures must stay in lockstep with it.
 */
interface FrameCallback {
    /** A connection was established with the given framebuffer geometry and the
     *  sender's quantization level count. */
    fun onConnect(width: Int, height: Int, levels: Int)

    /**
     * A decoded frame is ready. [pixels] is a **direct** ByteBuffer over the
     * core's reused Gray8 framebuffer (`width * height` bytes, row-major); it is
     * valid only for the duration of this call. Copy what you need and return
     * promptly — **do not retain the buffer**. Returning is also the pull
     * back-pressure: the core sends its next request only after this returns, so
     * a slow draw throttles the sender.
     *
     * [refreshHint]: 0 = Auto, 1 = Fast, 2 = Quality. Map it to a device
     * waveform in the [RefreshBackend]; the core never interprets it.
     */
    fun onFrame(pixels: ByteBuffer, width: Int, height: Int, refreshHint: Int)

    /** The active display mode changed (informational). */
    fun onModeChanged(name: String)

    /** An established connection ended. Fires on a clean [RecvCore.stop] too, not
     *  only on error — treat it as "connection ended", not "failure". */
    fun onDisconnect()
}

/**
 * Thin Kotlin handle over the native receiver core. Owns one background receiver
 * thread; [start] connects and [stop] tears down. No protocol or decode logic
 * lives in Kotlin — it's all in the loaded `.so`.
 */
class RecvCore {
    private var handle: Long = 0

    /** Connect to [addr] (e.g. `127.0.0.1:5920`) and begin delivering frames to
     *  [callback]. Idempotent: a second call while running is a no-op. */
    @Synchronized
    fun start(addr: String, callback: FrameCallback) {
        if (handle == 0L) {
            handle = nativeStart(addr, callback)
        }
    }

    /**
     * Stop the receiver and free native resources. Safe to call more than once.
     *
     * May block up to ~3 s if a connection attempt is in flight (the native
     * connect timeout is not interruptible), so call it **off the UI thread**.
     */
    @Synchronized
    fun stop() {
        val h = handle
        handle = 0
        if (h != 0L) {
            nativeStop(h)
        }
    }

    private external fun nativeStart(addr: String, callback: FrameCallback): Long
    private external fun nativeStop(handle: Long)

    companion object {
        init {
            System.loadLibrary("papercast_recv_core")
        }
    }
}
