package com.cakevpn.app

import android.app.Activity
import android.content.Intent
import android.net.Uri
import android.net.VpnService
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
