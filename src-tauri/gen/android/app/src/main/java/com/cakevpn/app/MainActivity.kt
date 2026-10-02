package com.cakevpn.app

import android.content.Intent
import android.os.Bundle
import android.view.View
import androidx.activity.enableEdgeToEdge
import androidx.core.view.ViewCompat
import androidx.core.view.WindowInsetsCompat

class MainActivity : TauriActivity() {
  companion object {
    /** What the Quick Settings tile opened CakeVPN for ("connect"). */
    const val EXTRA_ACTION = "com.cakevpn.app.ACTION"
  }

  private fun takeAction(intent: Intent?) {
    intent?.getStringExtra(EXTRA_ACTION)?.let { VpnPlugin.pendingAction = it }
    intent?.removeExtra(EXTRA_ACTION)
  }

  override fun onNewIntent(intent: Intent) {
    super.onNewIntent(intent)
    takeAction(intent)
  }

  override fun onCreate(savedInstanceState: Bundle?) {
    takeAction(intent)
    enableEdgeToEdge()
    super.onCreate(savedInstanceState)
    // Keep the app clear of the status bar, the navigation bar and camera
    // cutouts; the window's own color shows behind them.
    val content = findViewById<View>(android.R.id.content)
    ViewCompat.setOnApplyWindowInsetsListener(content) { view, insets ->
      val bars = insets.getInsets(
        WindowInsetsCompat.Type.systemBars() or WindowInsetsCompat.Type.displayCutout() or WindowInsetsCompat.Type.ime()
      )
      view.setPadding(bars.left, bars.top, bars.right, bars.bottom)
      WindowInsetsCompat.CONSUMED
    }
  }
}
