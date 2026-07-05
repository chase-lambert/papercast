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
 * No protocol knowledge lives here: it only reshapes bytes and draws. Panel-
 * specific refresh handling is delegated to [backend].
 */
class FrameRenderer(private val backend: RefreshBackend) : FrameCallback {

    // Guarded by `this`. `holder` is set from the UI thread (surface lifecycle)
    // and read on the receiver thread (onFrame), so both go through the lock.
    private var holder: SurfaceHolder? = null
    private var bitmap: Bitmap? = null
    private var argb: IntArray = IntArray(0)
    private var gray: ByteArray = ByteArray(0)
    private val paint = Paint(Paint.FILTER_BITMAP_FLAG)

    /** Called from the Activity as the surface appears/resizes/disappears. */
    @Synchronized
    fun setSurface(holder: SurfaceHolder?) {
        this.holder = holder
    }

    @Synchronized
    override fun onConnect(width: Int, height: Int, levels: Int) {
        // (Re)size the reusable buffers for this connection's geometry.
        bitmap = Bitmap.createBitmap(width, height, Bitmap.Config.ARGB_8888)
        argb = IntArray(width * height)
        gray = ByteArray(width * height)
    }

    @Synchronized
    override fun onFrame(pixels: ByteBuffer, width: Int, height: Int, refreshHint: Int) {
        val bmp = bitmap ?: return
        val target = holder ?: return // no surface yet — drop this frame, draw the next
        val count = width * height
        if (argb.size != count || gray.size != count) return

        // Copy out of the direct buffer (must not outlive this call) and expand
        // each Gray8 byte to an opaque ARGB pixel.
        pixels.get(gray, 0, count)
        for (i in 0 until count) {
            val v = gray[i].toInt() and 0xFF
            argb[i] = (0xFF shl 24) or (v shl 16) or (v shl 8) or v
        }
        bmp.setPixels(argb, 0, width, 0, 0, width, height)

        backend.applyHint(refreshHint)

        val canvas = target.lockCanvas() ?: return
        try {
            canvas.drawColor(Color.BLACK)
            // Letterbox: preserve aspect ratio, center in the surface.
            val scale = min(canvas.width.toFloat() / width, canvas.height.toFloat() / height)
            val dw = width * scale
            val dh = height * scale
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
