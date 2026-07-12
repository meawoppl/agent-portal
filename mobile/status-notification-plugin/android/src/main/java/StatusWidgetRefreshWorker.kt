package io.txcl.agentportal.status

import android.content.Context
import androidx.work.BackoffPolicy
import androidx.work.Constraints
import androidx.work.CoroutineWorker
import androidx.work.ExistingWorkPolicy
import androidx.work.NetworkType
import androidx.work.OneTimeWorkRequestBuilder
import androidx.work.PeriodicWorkRequestBuilder
import androidx.work.WorkManager
import androidx.work.WorkerParameters
import java.net.HttpURLConnection
import java.net.URL
import java.util.concurrent.TimeUnit

class StatusWidgetRefreshWorker(
    appContext: Context,
    workerParams: WorkerParameters,
) : CoroutineWorker(appContext, workerParams) {
    override suspend fun doWork(): Result {
        val config = StatusPayloadStore.loadRefreshConfig(applicationContext) ?: return Result.success()
        return try {
            val response = fetchStatus(config)
            if (response.statusCode == HttpURLConnection.HTTP_OK) {
                val hasSessions = StatusPayloadStore.saveFromStatusResponse(
                    context = applicationContext,
                    dashboardUrl = config.dashboardUrl,
                    statusUrl = config.statusUrl,
                    authToken = config.authToken,
                    responseBody = response.body,
                )
                StatusWidgetProvider.updateAll(applicationContext)
                if (hasSessions) Result.success() else {
                    cancel(applicationContext)
                    Result.success()
                }
            } else if (response.statusCode == HttpURLConnection.HTTP_UNAUTHORIZED ||
                response.statusCode == HttpURLConnection.HTTP_FORBIDDEN
            ) {
                StatusPayloadStore.clear(applicationContext)
                StatusWidgetProvider.updateAll(applicationContext)
                cancel(applicationContext)
                Result.success()
            } else {
                Result.retry()
            }
        } catch (_: Exception) {
            Result.retry()
        }
    }

    private fun fetchStatus(config: RefreshConfig): StatusResponse {
        val connection = (URL(config.statusUrl).openConnection() as HttpURLConnection).apply {
            requestMethod = "GET"
            setRequestProperty("Authorization", "Bearer ${config.authToken}")
            connectTimeout = 10_000
            readTimeout = 10_000
        }
        return try {
            val body = if (connection.responseCode in 200..299) {
                connection.inputStream.bufferedReader().use { it.readText() }
            } else {
                connection.errorStream?.bufferedReader()?.use { it.readText() }.orEmpty()
            }
            StatusResponse(connection.responseCode, body)
        } finally {
            connection.disconnect()
        }
    }

    private data class StatusResponse(
        val statusCode: Int,
        val body: String,
    )

    companion object {
        private const val UNIQUE_PERIODIC_WORK = "agent_portal_status_widget_periodic_refresh"
        private const val UNIQUE_IMMEDIATE_WORK = "agent_portal_status_widget_immediate_refresh"

        private val networkConstraints = Constraints.Builder()
            .setRequiredNetworkType(NetworkType.CONNECTED)
            .build()

        fun schedule(context: Context) {
            val periodic = PeriodicWorkRequestBuilder<StatusWidgetRefreshWorker>(
                15,
                TimeUnit.MINUTES,
            )
                .setConstraints(networkConstraints)
                .setBackoffCriteria(BackoffPolicy.EXPONENTIAL, 30, TimeUnit.SECONDS)
                .build()
            WorkManager.getInstance(context).enqueueUniquePeriodicWork(
                UNIQUE_PERIODIC_WORK,
                androidx.work.ExistingPeriodicWorkPolicy.REPLACE,
                periodic,
            )
        }

        fun refreshNow(context: Context) {
            val request = OneTimeWorkRequestBuilder<StatusWidgetRefreshWorker>()
                .setConstraints(networkConstraints)
                .setBackoffCriteria(BackoffPolicy.EXPONENTIAL, 30, TimeUnit.SECONDS)
                .build()
            WorkManager.getInstance(context).enqueueUniqueWork(
                UNIQUE_IMMEDIATE_WORK,
                ExistingWorkPolicy.REPLACE,
                request,
            )
        }

        fun cancel(context: Context) {
            WorkManager.getInstance(context).cancelUniqueWork(UNIQUE_IMMEDIATE_WORK)
            WorkManager.getInstance(context).cancelUniqueWork(UNIQUE_PERIODIC_WORK)
        }
    }
}
