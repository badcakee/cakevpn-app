package com.cakevpn.app

import android.app.PendingIntent
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.net.VpnService
import android.os.Build
import android.os.ParcelFileDescriptor

/**
 * The phone's VPN: a network interface that carries all traffic of the
 * phone except CakeVPN's own. The app's Rust side reads it (tun2proxy) and
 * passes everything to sing-box, whose connection to the server is CakeVPN's
 * own traffic and so goes out over the normal network.
 *
 * Android keeps this service running while the interface is open, also when
 * the app's window is closed.
 */
class CakeVpnService : VpnService() {

    companion object {
        private const val ACTION_START = "com.cakevpn.app.START"

        private var pending: ((Int?, String?) -> Unit)? = null
        private var skipApps: List<String> = emptyList()
        private var running: CakeVpnService? = null

        /** Opens the interface; `done` gets its file descriptor, or why it failed. */
        fun start(context: Context, skip: List<String>, done: (Int?, String?) -> Unit) {
            pending = done
            skipApps = skip
            try {
                context.startService(Intent(context, CakeVpnService::class.java).setAction(ACTION_START))
            } catch (e: Exception) {
                pending = null
                done(null, "Android didn't let CakeVPN start the VPN: ${e.message}")
            }
        }

        fun stop() {
            running?.close()
        }
    }

    private var tun: ParcelFileDescriptor? = null

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        if (intent?.action == ACTION_START) {
            open()
        } else if (tun == null) {
            // Started by the system (not by the app): nothing to carry the traffic to.
            stopSelf()
        }
        return START_NOT_STICKY
    }

    private fun open() {
        val done = pending
        pending = null
        try {
            val builder = Builder()
                .setSession("CakeVPN")
                .setMtu(1500)
                .addAddress("172.19.0.1", 30)
                .addRoute("0.0.0.0", 0)
                // IPv6 goes into the VPN too, so nothing can go around it; the
                // VPN refuses it unless the plan has IPv6, and apps use IPv4.
                .addAddress("fdfe:dcba:9876::1", 126)
                .addRoute("::", 0)
                .addDnsServer("172.19.0.2")
                // CakeVPN's own connection to the server stays outside.
                .addDisallowedApplication(packageName)
            for (app in skipApps) {
                try {
                    builder.addDisallowedApplication(app)
                } catch (_: PackageManager.NameNotFoundException) {
                    // Not installed on this phone: nothing to skip.
                }
            }
            val openApp = Intent(this, MainActivity::class.java)
            builder.setConfigureIntent(PendingIntent.getActivity(this, 0, openApp, PendingIntent.FLAG_IMMUTABLE))
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
                builder.setMetered(false)
            }
            val opened = builder.establish() ?: throw IllegalStateException("CakeVPN isn't allowed to set up a VPN.")
            tun?.close()
            tun = opened
            running = this
            done?.invoke(opened.fd, null)
        } catch (e: Exception) {
            done?.invoke(null, e.message ?: "The VPN could not be started.")
            if (tun == null) stopSelf()
        }
    }

    /** Closes the interface, which ends the VPN, and stops the service. */
    fun close() {
        try {
            tun?.close()
        } catch (_: Exception) {
        }
        tun = null
        if (running === this) running = null
        stopSelf()
    }

    /** Another VPN app took over, or the person turned CakeVPN off in Android's settings. */
    override fun onRevoke() {
        close()
    }

    override fun onDestroy() {
        close()
        super.onDestroy()
    }
}
