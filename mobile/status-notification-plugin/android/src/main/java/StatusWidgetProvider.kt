package io.txcl.agentportal.status

import android.app.PendingIntent
import android.appwidget.AppWidgetManager
import android.appwidget.AppWidgetProvider
import android.content.ComponentName
import android.content.Context
import android.content.Intent
import android.net.Uri
import android.view.View
import android.widget.RemoteViews

class StatusWidgetProvider : AppWidgetProvider() {
    override fun onUpdate(
        context: Context,
        appWidgetManager: AppWidgetManager,
        appWidgetIds: IntArray,
    ) {
        appWidgetIds.forEach { appWidgetId ->
            updateWidget(context, appWidgetManager, appWidgetId)
        }
    }

    companion object {
        private const val MAX_WIDGET_LINES = 3

        fun updateAll(context: Context) {
            val manager = AppWidgetManager.getInstance(context)
            val component = ComponentName(context, StatusWidgetProvider::class.java)
            val widgetIds = manager.getAppWidgetIds(component)
            widgetIds.forEach { widgetId ->
                updateWidget(context, manager, widgetId)
            }
        }

        private fun updateWidget(
            context: Context,
            manager: AppWidgetManager,
            widgetId: Int,
        ) {
            val payload = StatusPayloadStore.load(context)
            val views = RemoteViews(context.packageName, R.layout.agent_portal_status_widget)
            val sessions = payload.sessions.take(MAX_WIDGET_LINES)
            val dashboardUrl = payload.dashboardUrl.ifBlank {
                sessions.firstOrNull()?.url.orEmpty()
            }

            views.setTextViewText(R.id.widget_title, "Agent Portal")
            views.setTextViewText(R.id.widget_summary, widgetSummary(payload, sessions))
            views.setOnClickPendingIntent(
                R.id.widget_root,
                deepLinkPendingIntent(context, dashboardUrl, widgetId * 10),
            )

            bindRow(
                context,
                views,
                R.id.widget_row_1,
                R.id.widget_row_1_name,
                R.id.widget_row_1_state,
                sessions.getOrNull(0),
                widgetId * 10 + 1,
            )
            bindRow(
                context,
                views,
                R.id.widget_row_2,
                R.id.widget_row_2_name,
                R.id.widget_row_2_state,
                sessions.getOrNull(1),
                widgetId * 10 + 2,
            )
            bindRow(
                context,
                views,
                R.id.widget_row_3,
                R.id.widget_row_3_name,
                R.id.widget_row_3_state,
                sessions.getOrNull(2),
                widgetId * 10 + 3,
            )

            manager.updateAppWidget(widgetId, views)
        }

        private fun widgetSummary(payload: StatusPayload, sessions: List<StatusSession>): String {
            if (sessions.isEmpty()) return "No active sessions"
            return payload.summary.ifBlank { "${sessions.size} active" }
        }

        private fun bindRow(
            context: Context,
            views: RemoteViews,
            rowId: Int,
            nameId: Int,
            stateId: Int,
            session: StatusSession?,
            requestCode: Int,
        ) {
            if (session == null) {
                views.setViewVisibility(rowId, View.GONE)
                return
            }
            views.setViewVisibility(rowId, View.VISIBLE)
            views.setTextViewText(nameId, session.name)
            views.setTextViewText(stateId, session.state)
            views.setOnClickPendingIntent(
                rowId,
                deepLinkPendingIntent(context, session.url, requestCode),
            )
        }

        private fun deepLinkPendingIntent(
            context: Context,
            url: String,
            requestCode: Int,
        ): PendingIntent {
            val intent = if (url.isBlank()) {
                context.packageManager.getLaunchIntentForPackage(context.packageName)
                    ?: Intent(Intent.ACTION_MAIN).setPackage(context.packageName)
            } else {
                Intent(Intent.ACTION_VIEW, Uri.parse(url)).apply {
                    setPackage(context.packageName)
                    addFlags(Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_SINGLE_TOP)
                }
            }
            return PendingIntent.getActivity(
                context,
                requestCode,
                intent,
                PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
            )
        }
    }
}
