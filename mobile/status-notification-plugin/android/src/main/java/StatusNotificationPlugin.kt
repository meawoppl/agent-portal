package io.txcl.agentportal.status

import android.Manifest
import android.app.Activity
import android.content.Intent
import android.content.pm.PackageManager
import android.os.Build
import androidx.core.app.ActivityCompat
import androidx.core.content.ContextCompat
import app.tauri.annotation.Command
import app.tauri.annotation.InvokeArg
import app.tauri.annotation.TauriPlugin
import app.tauri.plugin.Invoke
import app.tauri.plugin.Plugin

@InvokeArg
class ShowArgs {
    lateinit var title: String
    lateinit var summary: String
    lateinit var dashboardUrl: String
    lateinit var sessionsJson: String
}

@TauriPlugin
class StatusNotificationPlugin(private val activity: Activity) : Plugin(activity) {
    @Command
    fun show(invoke: Invoke) {
        try {
            if (!ensureNotificationPermission()) {
                invoke.reject("notification permission has not been granted")
                return
            }
            val args = invoke.parseArgs(ShowArgs::class.java)
            val intent = Intent(activity, StatusNotificationService::class.java).apply {
                action = StatusNotificationService.ACTION_SHOW
                putExtra(StatusNotificationService.EXTRA_TITLE, args.title)
                putExtra(StatusNotificationService.EXTRA_SUMMARY, args.summary)
                putExtra(StatusNotificationService.EXTRA_DASHBOARD_URL, args.dashboardUrl)
                putExtra(StatusNotificationService.EXTRA_SESSIONS_JSON, args.sessionsJson)
            }
            ContextCompat.startForegroundService(activity, intent)
            invoke.resolve()
        } catch (ex: Exception) {
            invoke.reject(ex.message)
        }
    }

    @Command
    fun clear(invoke: Invoke) {
        try {
            val intent = Intent(activity, StatusNotificationService::class.java).apply {
                action = StatusNotificationService.ACTION_CLEAR
            }
            activity.startService(intent)
            invoke.resolve()
        } catch (ex: Exception) {
            invoke.reject(ex.message)
        }
    }

    private fun ensureNotificationPermission(): Boolean {
        if (Build.VERSION.SDK_INT < 33) return true
        if (ContextCompat.checkSelfPermission(activity, Manifest.permission.POST_NOTIFICATIONS) ==
            PackageManager.PERMISSION_GRANTED
        ) {
            return true
        }
        ActivityCompat.requestPermissions(
            activity,
            arrayOf(Manifest.permission.POST_NOTIFICATIONS),
            NOTIFICATION_PERMISSION_REQUEST
        )
        return false
    }

    companion object {
        private const val NOTIFICATION_PERMISSION_REQUEST = 4207
    }
}
