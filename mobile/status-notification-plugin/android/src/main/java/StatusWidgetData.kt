package io.txcl.agentportal.status

import android.content.Context
import org.json.JSONArray

data class StatusSession(
    val name: String,
    val state: String,
    val url: String,
) {
    fun actionLabel(): String = "$name: $state".take(32)
}

data class StatusPayload(
    val summary: String,
    val dashboardUrl: String,
    val sessions: List<StatusSession>,
)

object StatusPayloadStore {
    private const val PREFS_NAME = "agent_portal_status_widget"
    private const val KEY_SUMMARY = "summary"
    private const val KEY_DASHBOARD_URL = "dashboard_url"
    private const val KEY_SESSIONS_JSON = "sessions_json"

    fun save(context: Context, summary: String, dashboardUrl: String, sessionsJson: String) {
        context.getSharedPreferences(PREFS_NAME, Context.MODE_PRIVATE)
            .edit()
            .putString(KEY_SUMMARY, summary)
            .putString(KEY_DASHBOARD_URL, dashboardUrl)
            .putString(KEY_SESSIONS_JSON, sessionsJson)
            .apply()
    }

    fun clear(context: Context) {
        context.getSharedPreferences(PREFS_NAME, Context.MODE_PRIVATE)
            .edit()
            .clear()
            .apply()
    }

    fun load(context: Context): StatusPayload {
        val prefs = context.getSharedPreferences(PREFS_NAME, Context.MODE_PRIVATE)
        val sessionsJson = prefs.getString(KEY_SESSIONS_JSON, "[]") ?: "[]"
        return StatusPayload(
            summary = prefs.getString(KEY_SUMMARY, "No active sessions") ?: "No active sessions",
            dashboardUrl = prefs.getString(KEY_DASHBOARD_URL, "") ?: "",
            sessions = parseSessions(sessionsJson),
        )
    }

    fun parseSessions(sessionsJson: String): List<StatusSession> {
        val sessions = mutableListOf<StatusSession>()
        try {
            val json = JSONArray(sessionsJson)
            for (index in 0 until json.length()) {
                val item = json.getJSONObject(index)
                val name = item.optString("name").ifBlank { "Session" }
                val state = item.optString("state").ifBlank { "working" }
                val url = item.optString("url")
                if (url.isBlank()) continue
                sessions.add(StatusSession(name, state, url))
            }
        } catch (_: Exception) {
            return emptyList()
        }
        return sessions
    }
}
