package io.txcl.agentportal.status

import com.google.firebase.messaging.FirebaseMessagingService
import com.google.firebase.messaging.RemoteMessage

class PortalFirebaseMessagingService : FirebaseMessagingService() {
    override fun onNewToken(token: String) {
        PortalFcmBridge.onNewToken(applicationContext, token)
    }

    override fun onMessageReceived(message: RemoteMessage) {
        if (message.data.isNotEmpty()) {
            PortalFcmBridge.onDataMessage(applicationContext)
        }
    }
}
