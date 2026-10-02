package com.cakevpn.app

import android.app.Activity
import android.content.Intent
import android.content.pm.PackageManager
import android.graphics.Bitmap
import android.graphics.Canvas
import android.net.Uri
import android.net.VpnService
import android.os.Build
import android.util.Base64
import app.tauri.plugin.JSArray
import java.io.ByteArrayOutputStream
import androidx.activity.result.ActivityResult
import app.tauri.annotation.ActivityCallback
import app.tauri.annotation.Command
import app.tauri.annotation.InvokeArg
import app.tauri.annotation.TauriPlugin
import app.tauri.plugin.Invoke
import app.tauri.plugin.JSObject
import app.tauri.plugin.Plugin

@InvokeArg
class StartArgs {
    /** Package names of apps that skip the VPN. */
    var skipApps: Array<String> = arrayOf()
}

@InvokeArg
class OpenArgs {
    var url: String = ""
}

/**
 * What the app's Rust side (src-tauri/src/phone.rs) asks of Android: the
 * permission to run a VPN, the VPN interface itself, and opening a link.
 */
@TauriPlugin
class VpnPlugin(private val activity: Activity) : Plugin(activity) {

    companion object {
        /** What the Quick Settings tile opened the app for ("connect"), until the app takes it. */
        @Volatile
        var pendingAction: String? = null
    }

    /** The tile's request, once. */
    @Command
    fun takeAction(invoke: Invoke) {
        val action = pendingAction
        pendingAction = null
        invoke.resolve(JSObject().put("action", action))
    }

    /** Whether the VPN is up, and whether the tile asked to turn it off. */
    @Command
    fun vpnState(invoke: Invoke) {
        invoke.resolve(JSObject().put("running", CakeVpnService.isRunning()).put("stopRequested", CakeVpnService.stopRequested))
    }

    /** The apps on this phone (those with an icon in the app list), for choosing which skip the VPN. */
    @Command
    fun listApps(invoke: Invoke) {
        Thread {
            try {
                val pm = activity.packageManager
                val launcher = Intent(Intent.ACTION_MAIN).addCategory(Intent.CATEGORY_LAUNCHER)
                val found = if (Build.VERSION.SDK_INT >= 33) {
                    pm.queryIntentActivities(launcher, PackageManager.ResolveInfoFlags.of(0))
                } else {
                    @Suppress("DEPRECATION")
                    pm.queryIntentActivities(launcher, 0)
                }
                val seen = HashSet<String>()
                val apps = JSArray()
                for (info in found.sortedBy { it.loadLabel(pm).toString().lowercase() }) {
                    val id = info.activityInfo.packageName
                    if (id == activity.packageName || !seen.add(id)) continue
                    apps.put(
                        JSObject()
                            .put("id", id)
                            .put("name", info.loadLabel(pm).toString())
                            .put("icon", iconOf(info.loadIcon(pm)))
                    )
                }
                invoke.resolve(JSObject().put("apps", apps))
            } catch (e: Exception) {
                invoke.reject("The list of apps couldn't be read.")
            }
        }.start()
    }

    /** A small PNG of an app's icon, as a data: address. */
    private fun iconOf(drawable: android.graphics.drawable.Drawable): String {
        val size = 72
        val bitmap = Bitmap.createBitmap(size, size, Bitmap.Config.ARGB_8888)
        drawable.setBounds(0, 0, size, size)
        drawable.draw(Canvas(bitmap))
        val out = ByteArrayOutputStream()
        bitmap.compress(Bitmap.CompressFormat.PNG, 100, out)
        return "data:image/png;base64," + Base64.encodeToString(out.toByteArray(), Base64.NO_WRAP)
    }

    /** Asks the person once whether CakeVPN may set up a VPN. */
    @Command
    fun prepare(invoke: Invoke) {
        val ask = VpnService.prepare(activity)
        if (ask == null) {
            invoke.resolve(JSObject().put("granted", true))
            return
        }
        startActivityForResult(invoke, ask, "prepared")
    }

    @ActivityCallback
    fun prepared(invoke: Invoke, result: ActivityResult) {
        invoke.resolve(JSObject().put("granted", result.resultCode == Activity.RESULT_OK))
    }

    /** Opens the VPN interface; answers with its file descriptor. */
    @Command
    fun start(invoke: Invoke) {
        val args = invoke.parseArgs(StartArgs::class.java)
        if (VpnService.prepare(activity) != null) {
            invoke.reject("CakeVPN isn't allowed to set up a VPN yet. Press Connect and choose OK.")
            return
        }
        CakeVpnService.start(activity, args.skipApps.toList()) { fd, error ->
            if (fd != null) {
                invoke.resolve(JSObject().put("fd", fd))
            } else {
                invoke.reject(error ?: "The VPN could not be started.")
            }
        }
    }

    @Command
    fun stop(invoke: Invoke) {
        CakeVpnService.stop()
        invoke.resolve()
    }

    /** Opens a link in the browser, like an update's download. */
    @Command
    fun openUrl(invoke: Invoke) {
        val args = invoke.parseArgs(OpenArgs::class.java)
        try {
            activity.startActivity(Intent(Intent.ACTION_VIEW, Uri.parse(args.url)).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK))
            invoke.resolve()
        } catch (e: Exception) {
            invoke.reject("No browser could open the download.")
        }
    }
}
