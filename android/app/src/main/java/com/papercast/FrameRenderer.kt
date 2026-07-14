package com.papercast

import android.graphics.Bitmap
import android.graphics.Color
import android.graphics.Paint
import android.graphics.RectF
import android.view.SurfaceHolder
import java.nio.ByteBuffer
import kotlin.math.min

/**
 * Turns decoded Gray8 frames into pixels on a [SurfaceHolder]. Implements
 * [FrameCallback], so its methods run on the receiver core's thread — drawing to
 * a SurfaceView off the UI thread is supported and is what keeps the pull loop's
 * back-pressure honest (we draw synchronously, then the core requests the next
 * frame).
 *
 * The newest frame is always decoded into a cached bitmap, even when no surface
 * is attached, and re-drawn whenever a surface (re)appears. That matters because
 * the protocol has no client-initiated resend: if the first full-quality paint
 * races surface creation, or the surface is recreated (e.g. on rotation), we'd
 * otherwise sit blank/stale until the next captured update — which on an
 * idle desktop can be a long time.
 *
 * No protocol knowledge lives here: it only reshapes bytes and draws. Panel-
 * specific refresh handling is delegated to [backend].
 */
class FrameRenderer(private val backend: RefreshBackend) : FrameCallback {

    // All fields are guarded by `this`. `holder` is set from the UI thread
    // (surface lifecycle) and read on the receiver thread (onFrame); the cached
    // bitmap is written on the receiver thread and read from both, so every
    // access goes through the lock.
    private var holder: SurfaceHolder? = null
    private var bitmap: Bitmap? = null
    private var argb: IntArray = IntArray(0)
    private var gray: ByteArray = ByteArray(0)
    private var hasFrame = false
    private var lastHint = 0
    private val paint = Paint(Paint.FILTER_BITMAP_FLAG)

    /** Called from the Activity as the surface appears/resizes/disappears. On a
     *  (re)appearance, repaint the latest cached frame so the screen isn't blank
     *  while we wait for the next update. */
    @Synchronized
    fun setSurface(holder: SurfaceHolder?) {
        this.holder = holder
        if (holder != null && hasFrame) {
            // A waveform-aware backend must force Quality for this cached,
            // full-screen repaint instead of replaying a possibly Fast hint.
            draw(holder, lastHint)
        }
    }

    @Synchronized
    override fun onConnect(width: Int, height: Int, levels: Int) {
        // (Re)size the reusable buffers for this connection's geometry.
        bitmap = Bitmap.createBitmap(width, height, Bitmap.Config.ARGB_8888)
        argb = IntArray(width * height)
        gray = ByteArray(width * height)
        hasFrame = false
    }

    @Synchronized
    override fun onFrame(pixels: ByteBuffer, width: Int, height: Int, refreshHint: Int) {
        val bmp = bitmap ?: return
        val count = width * height
        if (argb.size != count || gray.size != count) return

        // Always decode into the cached bitmap, even with no surface attached, so
        // a surface that appears later can be painted immediately. Copy out of the
        // direct buffer (it must not outlive this call) and expand each Gray8 byte
        // to an opaque ARGB pixel.
        pixels.get(gray, 0, count)
        for (i in 0 until count) {
            val v = gray[i].toInt() and 0xFF
            argb[i] = (0xFF shl 24) or (v shl 16) or (v shl 8) or v
        }
        bmp.setPixels(argb, 0, width, 0, 0, width, height)
        hasFrame = true
        lastHint = refreshHint

        holder?.let { draw(it, refreshHint) }
    }

    /** Draw the cached bitmap to [target], letterboxed and centered. Callers hold
     *  the monitor, so this never races another draw. */
    private fun draw(target: SurfaceHolder, hint: Int) {
        val bmp = bitmap ?: return
        backend.applyHint(hint)
        val canvas = target.lockCanvas() ?: return
        try {
            canvas.drawColor(Color.BLACK)
            val scale = min(canvas.width.toFloat() / bmp.width, canvas.height.toFloat() / bmp.height)
            val dw = bmp.width * scale
            val dh = bmp.height * scale
            val left = (canvas.width - dw) / 2f
            val top = (canvas.height - dh) / 2f
            canvas.drawBitmap(bmp, null, RectF(left, top, left + dw, top + dh), paint)
        } finally {
            target.unlockCanvasAndPost(canvas)
        }
    }

    override fun onModeChanged(name: String) {
        // Informational; the per-frame hint already carries all refresh intent.
    }

    override fun onDisconnect() {
        // Connection ended (including on a clean stop); nothing to do here.
    }
}
