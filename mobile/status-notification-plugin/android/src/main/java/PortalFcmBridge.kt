package io.txcl.agentportal.status

import android.content.Context
import android.os.Build
import com.google.firebase.FirebaseApp
import com.google.firebase.messaging.FirebaseMessaging
import java.net.HttpURLConnection
import java.net.URL
import org.json.JSONObject

object PortalFcmBridge {
    private const val PREFS_NAME = "agent_portal_fcm"
    private const val KEY_BACKEND_URL = "backend_url"
    private const val KEY_AUTH_TOKEN = "auth_token"
    private const val KEY_DEVICE_LABEL = "device_label"
    private const val KEY_SUBSCRIPTION_ID = "subscription_id"
    private const val KEY_FCM_TOKEN = "fcm_token"

    fun register(context: Context, backendUrl: String, authToken: String, deviceLabel: String) {
        saveConfig(context, backendUrl, authToken, deviceLabel)
        if (!firebaseAvailable(context)) return

        FirebaseMessaging.getInstance().token.addOnCompleteListener { task ->
            if (!task.isSuccessful) return@addOnCompleteListener
            val fcmToken = task.result ?: return@addOnCompleteListener
            registerTokenAsync(context.applicationContext, fcmToken)
        }
    }

    fun unregister(context: Context) {
        val config = loadConfig(context)
        val subscriptionId = context.getSharedPreferences(PREFS_NAME, Context.MODE_PRIVATE)
            .getString(KEY_SUBSCRIPTION_ID, "")
            .orEmpty()
        if (config != null && subscriptionId.isNotBlank()) {
            Thread {
                runCatching { deleteSubscription(config, subscriptionId) }
                clear(context)
            }.start()
        } else {
            clear(context)
        }
        if (firebaseAvailable(context)) {
            FirebaseMessaging.getInstance().deleteToken()
        }
    }

    fun onNewToken(context: Context, fcmToken: String) {
        registerTokenAsync(context.applicationContext, fcmToken)
    }

    fun onDataMessage(context: Context) {
        StatusWidgetRefreshWorker.refreshNow(context)
    }

    private fun registerTokenAsync(context: Context, fcmToken: String) {
        val config = loadConfig(context) ?: return
        Thread {
            runCatching {
                val subscriptionId = postSubscription(config, fcmToken)
                context.getSharedPreferences(PREFS_NAME, Context.MODE_PRIVATE)
                    .edit()
                    .putString(KEY_FCM_TOKEN, fcmToken)
                    .putString(KEY_SUBSCRIPTION_ID, subscriptionId)
                    .apply()
            }
        }.start()
    }

    private fun firebaseAvailable(context: Context): Boolean {
        return try {
            if (FirebaseApp.getApps(context).isNotEmpty()) return true
            FirebaseApp.initializeApp(context) != null
        } catch (_: Exception) {
            false
        }
    }

    private fun saveConfig(
        context: Context,
        backendUrl: String,
        authToken: String,
        deviceLabel: String,
    ) {
        context.getSharedPreferences(PREFS_NAME, Context.MODE_PRIVATE)
            .edit()
            .putString(KEY_BACKEND_URL, backendUrl.trimEnd('/'))
            .putString(KEY_AUTH_TOKEN, authToken)
            .putString(KEY_DEVICE_LABEL, deviceLabel.ifBlank { defaultDeviceLabel() })
            .apply()
    }

    private fun clear(context: Context) {
        context.getSharedPreferences(PREFS_NAME, Context.MODE_PRIVATE)
            .edit()
            .clear()
            .apply()
    }

    private fun loadConfig(context: Context): FcmConfig? {
        val prefs = context.getSharedPreferences(PREFS_NAME, Context.MODE_PRIVATE)
        val backendUrl = prefs.getString(KEY_BACKEND_URL, "") ?: ""
        val authToken = prefs.getString(KEY_AUTH_TOKEN, "") ?: ""
        val deviceLabel = prefs.getString(KEY_DEVICE_LABEL, "") ?: ""
        if (backendUrl.isBlank() || authToken.isBlank()) return null
        return FcmConfig(
            backendUrl = backendUrl.trimEnd('/'),
            authToken = authToken,
            deviceLabel = deviceLabel.ifBlank { defaultDeviceLabel() },
        )
    }

    private fun postSubscription(config: FcmConfig, fcmToken: String): String {
        val connection = (URL("${config.backendUrl}/api/push/subscriptions").openConnection() as HttpURLConnection)
            .apply {
                requestMethod = "POST"
                doOutput = true
                connectTimeout = 10_000
                readTimeout = 10_000
                setRequestProperty("Authorization", "Bearer ${config.authToken}")
                setRequestProperty("Content-Type", "application/json")
            }
        val body = JSONObject()
            .put("platform", "fcm")
            .put("endpoint_or_token", fcmToken)
            .put("p256dh", JSONObject.NULL)
            .put("auth", JSONObject.NULL)
            .put("device_label", config.deviceLabel)
            .toString()
        return try {
            connection.outputStream.use { output ->
                output.write(body.toByteArray(Charsets.UTF_8))
            }
            val responseBody = if (connection.responseCode in 200..299) {
                connection.inputStream.bufferedReader().use { it.readText() }
            } else {
                connection.errorStream?.bufferedReader()?.use { it.readText() }.orEmpty()
            }
            if (connection.responseCode !in 200..299) {
                throw IllegalStateException("FCM subscription returned ${connection.responseCode}")
            }
            JSONObject(responseBody).optString("id")
        } finally {
            connection.disconnect()
        }
    }

    private fun deleteSubscription(config: FcmConfig, subscriptionId: String) {
        val connection = (URL("${config.backendUrl}/api/push/subscriptions/$subscriptionId").openConnection() as HttpURLConnection)
            .apply {
                requestMethod = "DELETE"
                connectTimeout = 10_000
                readTimeout = 10_000
                setRequestProperty("Authorization", "Bearer ${config.authToken}")
            }
        try {
            connection.responseCode
        } finally {
            connection.disconnect()
        }
    }

    private fun defaultDeviceLabel(): String {
        val model = listOf(Build.MANUFACTURER, Build.MODEL)
            .filter { it.isNotBlank() }
            .joinToString(" ")
            .ifBlank { "Android" }
        return "Agent Portal Android ($model)"
    }

    private data class FcmConfig(
        val backendUrl: String,
        val authToken: String,
        val deviceLabel: String,
    )
}
