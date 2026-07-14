package com.papercast

import android.app.Activity
import android.os.Bundle
import android.view.SurfaceHolder
import android.view.SurfaceView
import android.view.View
import android.view.WindowManager

/**
 * The whole app: a fullscreen [SurfaceView] that mirrors the host. Lifecycle glue
 * only — connect on resume, stop on pause — with all protocol/decode work in the
 * native core and all drawing in [FrameRenderer].
 *
 * The host is reached at `127.0.0.1:5920`, bridged over USB with
 * `adb reverse tcp:5920 tcp:5920`. Setup is documented under “Experimental
 * native receiver” in the repository's root README.
 */
class MainActivity : Activity(), SurfaceHolder.Callback {

    private val core = RecvCore()
    private lateinit var renderer: FrameRenderer
    private lateinit var surface: SurfaceView

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        window.addFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON)

        // Manual backend override via `adb shell am start ... --es backend generic`.
        val override = intent?.getStringExtra(EXTRA_BACKEND)
        renderer = FrameRenderer(RefreshBackend.select(override))

        surface = SurfaceView(this)
        surface.holder.addCallback(this)
        setContentView(surface)
    }

    override fun onWindowFocusChanged(hasFocus: Boolean) {
        super.onWindowFocusChanged(hasFocus)
        if (hasFocus) enterImmersiveMode()
    }

    override fun onResume() {
        super.onResume()
        core.start(HOST_ADDR, renderer)
    }

    override fun onPause() {
        super.onPause()
        // nativeStop can block up to ~3 s mid-connect, so never on the UI thread.
        Thread { core.stop() }.start()
    }

    // --- SurfaceHolder.Callback: the renderer draws only while a surface exists ---

    override fun surfaceCreated(holder: SurfaceHolder) {
        renderer.setSurface(holder)
    }

    override fun surfaceChanged(holder: SurfaceHolder, format: Int, width: Int, height: Int) {
        renderer.setSurface(holder)
    }

    override fun surfaceDestroyed(holder: SurfaceHolder) {
        renderer.setSurface(null)
    }

    @Suppress("DEPRECATION")
    private fun enterImmersiveMode() {
        // WindowInsetsController is API 30+, but minSdk is 26, so use the older
        // systemUiVisibility flags — still functional across the supported range.
        surface.systemUiVisibility = (
            View.SYSTEM_UI_FLAG_IMMERSIVE_STICKY
                or View.SYSTEM_UI_FLAG_FULLSCREEN
                or View.SYSTEM_UI_FLAG_HIDE_NAVIGATION
                or View.SYSTEM_UI_FLAG_LAYOUT_STABLE
                or View.SYSTEM_UI_FLAG_LAYOUT_FULLSCREEN
                or View.SYSTEM_UI_FLAG_LAYOUT_HIDE_NAVIGATION
            )
    }

    companion object {
        private const val HOST_ADDR = "127.0.0.1:5920"
        private const val EXTRA_BACKEND = "backend"
    }
}
