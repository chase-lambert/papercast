package com.papercast

import android.os.Build

/**
 * The device-abstraction seam. The receiver core and wire protocol are
 * device-neutral: a frame carries only a *refresh intent* (Auto / Fast /
 * Quality), never a vendor waveform. A `RefreshBackend` is the one place that
 * turns that intent into whatever the panel needs.
 *
 * [GenericRefreshBackend] draws and ignores the hint. Hardware that needs
 * waveform control gets a vendor implementation behind this interface without
 * changing the receiver core or wire protocol.
 */
interface RefreshBackend {
    /**
     * Apply the device's refresh policy for the frame about to be drawn, given
     * its [hint] (0 = Auto, 1 = Fast, 2 = Quality). Called on the render thread
     * immediately before the draw. The generic backend does nothing; a vendor
     * backend sets the EPD waveform here.
     */
    fun applyHint(hint: Int)

    companion object {
        /** Names accepted as a manual override (see [select]). */
        const val GENERIC = "generic"

        /**
         * Choose a backend. [override] (e.g. from an intent extra) wins if it
         * names a known backend; otherwise the choice is by manufacturer, with
         * [GenericRefreshBackend] as the always-available fallback.
         */
        fun select(override: String?): RefreshBackend {
            return when (override?.lowercase()) {
                GENERIC -> GenericRefreshBackend()
                null -> byManufacturer()
                else -> byManufacturer() // unknown override → fall back
            }
        }

        private fun byManufacturer(): RefreshBackend {
            return when (Build.MANUFACTURER.lowercase()) {
                else -> GenericRefreshBackend()
            }
        }
    }
}

/** Draws every frame with no special panel handling. */
class GenericRefreshBackend : RefreshBackend {
    override fun applyHint(hint: Int) {
        // Nothing to do: on a generic panel the draw itself is the refresh.
    }
}
