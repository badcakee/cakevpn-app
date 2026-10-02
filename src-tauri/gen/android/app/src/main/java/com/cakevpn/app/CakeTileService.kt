package com.cakevpn.app

import android.app.PendingIntent
import android.content.ComponentName
import android.content.Context
import android.content.Intent
import android.os.Handler
import android.os.Looper
import android.os.Build
import android.service.quicksettings.Tile
import android.service.quicksettings.TileService

/**
 * The CakeVPN switch in Android's Quick Settings (the pull-down menu).
 * On: the VPN is up, and a tap turns it off. Off: a tap opens CakeVPN and
 * connects (the connection needs the app: the account, the location, the
 * settings).
 */
class CakeTileService : TileService() {

    companion object {
        /** The tile while Android shows it, so a change can update it right away. */
        @Volatile
        private var live: CakeTileService? = null

        /** Shows the VPN as it is now: directly while the tile is up, otherwise when Android next asks. */
        fun refresh(context: Context) {
            Handler(Looper.getMainLooper()).post {
                live?.show()
                try {
                    requestListeningState(context, ComponentName(context, CakeTileService::class.java))
                } catch (_: Exception) {
                }
            }
        }
    }

    override fun onStartListening() {
        super.onStartListening()
        live = this
        show()
    }

    override fun onStopListening() {
        if (live === this) live = null
        super.onStopListening()
    }

    override fun onClick() {
        super.onClick()
        if (CakeVpnService.isRunning()) {
            // The app takes the tunnel down the normal way within a second.
            CakeVpnService.stopRequested = true
            show(off = true)
            return
        }
        val open = Intent(this, MainActivity::class.java)
            .putExtra(MainActivity.EXTRA_ACTION, "connect")
            .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_SINGLE_TOP)
        if (Build.VERSION.SDK_INT >= 34) {
            startActivityAndCollapse(
                PendingIntent.getActivity(this, 1, open, PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT)
            )
        } else {
            @Suppress("DEPRECATION")
            startActivityAndCollapse(open)
        }
    }

    fun show(off: Boolean = false) {
        val tile = qsTile ?: return
        val on = !off && CakeVpnService.isRunning() && !CakeVpnService.stopRequested
        tile.state = if (on) Tile.STATE_ACTIVE else Tile.STATE_INACTIVE
        tile.label = "CakeVPN"
        if (Build.VERSION.SDK_INT >= 29) {
            tile.subtitle = if (on) getString(R.string.tile_on) else getString(R.string.tile_off)
        }
        tile.updateTile()
    }
}
