package io.txcl.agentportal.status

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.Context
import android.content.Intent
import android.net.Uri
import android.os.Build
import android.os.IBinder
import androidx.core.app.NotificationCompat
import org.json.JSONArray

class StatusNotificationService : Service() {
    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        when (intent?.action) {
            ACTION_CLEAR -> {
                if (Build.VERSION.SDK_INT >= 24) {
                    stopForeground(STOP_FOREGROUND_REMOVE)
                } else {
                    @Suppress("DEPRECATION")
                    stopForeground(true)
                }
                stopSelf()
            }
            ACTION_SHOW -> {
                val notification = buildNotification(
                    context = this,
                    title = intent.getStringExtra(EXTRA_TITLE) ?: "Agent Portal",
                    summary = intent.getStringExtra(EXTRA_SUMMARY) ?: "",
                    dashboardUrl = intent.getStringExtra(EXTRA_DASHBOARD_URL) ?: "",
                    sessionsJson = intent.getStringExtra(EXTRA_SESSIONS_JSON) ?: "[]",
                )
                startForeground(NOTIFICATION_ID, notification)
            }
        }
        return START_STICKY
    }

    override fun onBind(intent: Intent?): IBinder? = null

    private fun buildNotification(
        context: Context,
        title: String,
        summary: String,
        dashboardUrl: String,
        sessionsJson: String,
    ): Notification {
        ensureChannel(context)
        val sessions = parseSessions(sessionsJson)
        val inbox = NotificationCompat.InboxStyle().setSummaryText(summary)
        sessions.take(MAX_LINES).forEach { session ->
            inbox.addLine("${session.name} - ${session.state}")
        }

        val contentUrl = sessions.firstOrNull()?.url ?: dashboardUrl
        val builder = NotificationCompat.Builder(context, CHANNEL_ID)
            .setSmallIcon(context.applicationInfo.icon)
            .setContentTitle(title)
            .setContentText(summary)
            .setStyle(inbox)
            .setOngoing(true)
            .setOnlyAlertOnce(true)
            .setShowWhen(false)
            .setPriority(NotificationCompat.PRIORITY_LOW)
            .setContentIntent(deepLinkPendingIntent(context, contentUrl, 0))

        sessions.take(MAX_ACTIONS).forEachIndexed { index, session ->
            builder.addAction(
                android.R.drawable.ic_menu_view,
                session.actionLabel(),
                deepLinkPendingIntent(context, session.url, index + 1),
            )
        }

        return builder.build()
    }

    private fun parseSessions(sessionsJson: String): List<StatusSession> {
        val sessions = mutableListOf<StatusSession>()
        val json = JSONArray(sessionsJson)
        for (index in 0 until json.length()) {
            val item = json.getJSONObject(index)
            val name = item.optString("name").ifBlank { "Session" }
            val state = item.optString("state").ifBlank { "working" }
            val url = item.optString("url")
            if (url.isBlank()) continue
            sessions.add(StatusSession(name, state, url))
        }
        return sessions
    }

    private fun ensureChannel(context: Context) {
        if (Build.VERSION.SDK_INT < 26) return
        val manager = context.getSystemService(NotificationManager::class.java)
        if (manager.getNotificationChannel(CHANNEL_ID) != null) return
        val channel = NotificationChannel(
            CHANNEL_ID,
            "Agent Portal status",
            NotificationManager.IMPORTANCE_LOW,
        ).apply {
            description = "Active Agent Portal session status"
            setShowBadge(false)
        }
        manager.createNotificationChannel(channel)
    }

    private fun deepLinkPendingIntent(
        context: Context,
        url: String,
        requestCode: Int,
    ): PendingIntent {
        val intent = Intent(Intent.ACTION_VIEW, Uri.parse(url)).apply {
            setPackage(context.packageName)
            addFlags(Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_SINGLE_TOP)
        }
        return PendingIntent.getActivity(
            context,
            requestCode,
            intent,
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
        )
    }

    private data class StatusSession(
        val name: String,
        val state: String,
        val url: String,
    ) {
        fun actionLabel(): String = "$name: $state".take(32)
    }

    companion object {
        const val ACTION_SHOW = "io.txcl.agentportal.status.SHOW"
        const val ACTION_CLEAR = "io.txcl.agentportal.status.CLEAR"
        const val EXTRA_TITLE = "title"
        const val EXTRA_SUMMARY = "summary"
        const val EXTRA_DASHBOARD_URL = "dashboard_url"
        const val EXTRA_SESSIONS_JSON = "sessions_json"

        private const val CHANNEL_ID = "agent_portal_status"
        private const val NOTIFICATION_ID = 2401
        private const val MAX_LINES = 5
        private const val MAX_ACTIONS = 3
    }
}
