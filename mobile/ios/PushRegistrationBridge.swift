// APNs registration bridge (mobile-apps plan §15, work item E4).
//
// Scope: turn an APNs device token into a portal push subscription —
// nothing else. Displaying notifications, badges, and deep-link routing
// belong to the shell's notification layer, not this file.
//
// Flow:
//   1. `start(config:)` asks for notification permission and, when granted,
//      registers with APNs on the main thread.
//   2. The generated AppDelegate forwards the two UIApplicationDelegate
//      callbacks here (see mobile/ios/README.md for the exact wiring).
//   3. The device token is hex-encoded and POSTed to
//      `POST {backend}/api/push/subscriptions` as
//      `{platform: "apns", endpoint_or_token: <hex>, device_label: …}`
//      with the shell's mobile JWT as a Bearer header — the same typed
//      contract (`RegisterPushSubscriptionRequest`) the Web Push and FCM
//      paths use.
//
// Registration is idempotent server-side (re-POSTing the same token
// re-enables a previously disabled row), so calling `start` on every app
// launch is correct: APNs may rotate the token at any time and the fresh
// value must win.
//
// NOT YET COMPILE-VERIFIED: there is no macOS builder or Apple credential
// set in CI (plan items F2/F3). This file is not referenced by any build
// until it is added to the generated Xcode project.

import Foundation
import UIKit
import UserNotifications

public final class PushRegistrationBridge: NSObject {
    /// Everything the bridge needs from the shell. The shell owns auth (the
    /// mobile JWT lives in its store, `AUTH_TOKEN_KEY`) — Swift receives the
    /// token by value at wiring time and never reads storage itself.
    public struct Config {
        /// Portal origin, e.g. `https://portal.example.com` (no trailing path).
        public let backendBaseURL: URL
        /// The shell's current mobile JWT for the `Authorization: Bearer` header.
        public let bearerToken: String

        public init(backendBaseURL: URL, bearerToken: String) {
            self.backendBaseURL = backendBaseURL
            self.bearerToken = bearerToken
        }
    }

    public static let shared = PushRegistrationBridge()

    private var config: Config?

    private override init() {
        super.init()
    }

    /// Request permission and register with APNs. Call once per launch after
    /// the shell has a valid mobile JWT (post auth-handoff).
    public func start(config: Config) {
        self.config = config
        UNUserNotificationCenter.current().requestAuthorization(
            options: [.alert, .badge, .sound]
        ) { granted, error in
            if let error = error {
                NSLog("PushRegistrationBridge: authorization error: %@", error.localizedDescription)
                return
            }
            guard granted else {
                NSLog("PushRegistrationBridge: notification permission denied")
                return
            }
            DispatchQueue.main.async {
                UIApplication.shared.registerForRemoteNotifications()
            }
        }
    }

    /// Forward from
    /// `application(_:didRegisterForRemoteNotificationsWithDeviceToken:)`.
    public func didRegister(deviceToken: Data) {
        let hexToken = deviceToken.map { String(format: "%02x", $0) }.joined()
        postSubscription(hexToken: hexToken)
    }

    /// Forward from
    /// `application(_:didFailToRegisterForRemoteNotificationsWithError:)`.
    public func didFailToRegister(error: Error) {
        NSLog("PushRegistrationBridge: APNs registration failed: %@", error.localizedDescription)
    }

    // MARK: - Backend registration

    private func postSubscription(hexToken: String) {
        guard let config = config else {
            NSLog("PushRegistrationBridge: token received before start(config:); dropping")
            return
        }

        let url = config.backendBaseURL
            .appendingPathComponent("api")
            .appendingPathComponent("push")
            .appendingPathComponent("subscriptions")

        var request = URLRequest(url: url)
        request.httpMethod = "POST"
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        request.setValue("Bearer \(config.bearerToken)", forHTTPHeaderField: "Authorization")

        // Mirrors shared::api::RegisterPushSubscriptionRequest — `p256dh` /
        // `auth` are Web Push-only and omitted for APNs.
        let body: [String: String] = [
            "platform": "apns",
            "endpoint_or_token": hexToken,
            "device_label": "\(UIDevice.current.name) (\(UIDevice.current.model))",
        ]
        guard let payload = try? JSONSerialization.data(withJSONObject: body) else {
            NSLog("PushRegistrationBridge: could not encode subscription payload")
            return
        }
        request.httpBody = payload

        URLSession.shared.dataTask(with: request) { _, response, error in
            if let error = error {
                NSLog("PushRegistrationBridge: subscription POST failed: %@", error.localizedDescription)
                return
            }
            guard let http = response as? HTTPURLResponse else { return }
            if (200..<300).contains(http.statusCode) {
                NSLog("PushRegistrationBridge: APNs subscription registered")
            } else {
                // 401 here means the JWT expired between wiring and delivery;
                // the shell refreshes on next launch and start() runs again.
                NSLog("PushRegistrationBridge: subscription POST returned %d", http.statusCode)
            }
        }.resume()
    }
}
