package io.txcl.agentportal.status

import android.content.Context
import android.net.Uri
import org.json.JSONArray
import org.json.JSONObject

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
    private const val KEY_STATUS_URL = "status_url"
    private const val KEY_AUTH_TOKEN = "auth_token"
    private const val KEY_SESSIONS_JSON = "sessions_json"

    fun save(
        context: Context,
        summary: String,
        dashboardUrl: String,
        statusUrl: String,
        authToken: String,
        sessionsJson: String,
    ) {
        context.getSharedPreferences(PREFS_NAME, Context.MODE_PRIVATE)
            .edit()
            .putString(KEY_SUMMARY, summary)
            .putString(KEY_DASHBOARD_URL, dashboardUrl)
            .putString(KEY_STATUS_URL, statusUrl)
            .putString(KEY_AUTH_TOKEN, authToken)
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

    fun loadRefreshConfig(context: Context): RefreshConfig? {
        val prefs = context.getSharedPreferences(PREFS_NAME, Context.MODE_PRIVATE)
        val statusUrl = prefs.getString(KEY_STATUS_URL, "") ?: ""
        val dashboardUrl = prefs.getString(KEY_DASHBOARD_URL, "") ?: ""
        val authToken = prefs.getString(KEY_AUTH_TOKEN, "") ?: ""
        if (statusUrl.isBlank() || dashboardUrl.isBlank() || authToken.isBlank()) return null
        return RefreshConfig(
            statusUrl = statusUrl,
            dashboardUrl = dashboardUrl,
            authToken = authToken,
        )
    }

    fun saveFromStatusResponse(
        context: Context,
        dashboardUrl: String,
        statusUrl: String,
        authToken: String,
        responseBody: String,
    ): Boolean {
        val lines = statusLinesFromResponse(dashboardUrl, responseBody)
        if (lines.isEmpty()) {
            clear(context)
            return false
        }
        val summary = statusSummary(lines)
        save(
            context = context,
            summary = summary,
            dashboardUrl = dashboardUrl,
            statusUrl = statusUrl,
            authToken = authToken,
            sessionsJson = statusLinesJson(lines),
        )
        return true
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

    private fun statusLinesFromResponse(dashboardUrl: String, responseBody: String): List<StatusSession> {
        val lines = mutableListOf<StatusSession>()
        try {
            val sessions = JSONObject(responseBody).optJSONArray("sessions") ?: return emptyList()
            for (index in 0 until sessions.length()) {
                val session = sessions.getJSONObject(index)
                if (!shouldShow(session)) continue
                val id = session.optString("id")
                if (id.isBlank()) continue
                lines.add(
                    StatusSession(
                        name = compactSessionName(session.optString("session_name")),
                        state = statusState(session),
                        url = dashboardSessionUrl(dashboardUrl, id),
                    )
                )
                if (lines.size >= MAX_STATUS_SESSIONS) break
            }
        } catch (_: Exception) {
            return emptyList()
        }
        return lines
    }

    private fun shouldShow(session: JSONObject): Boolean {
        val status = session.optString("status").lowercase()
        return session.optBoolean("awaiting_permission", false) ||
            status.contains("active") ||
            status.contains("working") ||
            status.contains("running") ||
            status.contains("disconnect")
    }

    private fun statusState(session: JSONObject): String {
        if (session.optBoolean("awaiting_permission", false)) return "awaiting input"
        return if (session.optString("status").lowercase().contains("disconnect")) {
            "disconnected"
        } else {
            "working"
        }
    }

    private fun compactSessionName(name: String): String {
        val trimmed = name.trim()
        if (trimmed.isBlank()) return "Session"
        return if (trimmed.length <= MAX_SESSION_NAME_CHARS) {
            trimmed
        } else {
            trimmed.take(MAX_SESSION_NAME_CHARS - 3) + "..."
        }
    }

    private fun dashboardSessionUrl(dashboardUrl: String, sessionId: String): String {
        return Uri.parse(dashboardUrl)
            .buildUpon()
            .encodedQuery(null)
            .appendQueryParameter("session", sessionId)
            .build()
            .toString()
    }

    private fun statusSummary(lines: List<StatusSession>): String {
        val awaiting = lines.count { it.state == "awaiting input" }
        val disconnected = lines.count { it.state == "disconnected" }
        return when {
            awaiting > 0 -> "$awaiting awaiting input"
            disconnected > 0 -> "$disconnected disconnected"
            else -> "${lines.size} working"
        }
    }

    private fun statusLinesJson(lines: List<StatusSession>): String {
        val array = JSONArray()
        lines.forEach { line ->
            array.put(
                JSONObject()
                    .put("name", line.name)
                    .put("state", line.state)
                    .put("url", line.url)
            )
        }
        return array.toString()
    }

    private const val MAX_STATUS_SESSIONS = 5
    private const val MAX_SESSION_NAME_CHARS = 32
}

data class RefreshConfig(
    val statusUrl: String,
    val dashboardUrl: String,
    val authToken: String,
)
