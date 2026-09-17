import Foundation
import UIKit
import UserNotifications

final class IOSClipboard: ClipboardBridge, @unchecked Sendable {
    private func onMain<T>(_ body: () -> T) -> T {
        if Thread.isMainThread { return body() }
        return DispatchQueue.main.sync(execute: body)
    }

    func readText() -> String? {
        onMain { UIPasteboard.general.hasStrings ? UIPasteboard.general.string : nil }
    }

    func writeText(text: String) {
        onMain { UIPasteboard.general.string = text }
    }

    func readImagePng() -> Data? {
        onMain { UIPasteboard.general.image?.pngData() }
    }

    func writeImagePng(png: Data) {
        onMain {
            if let image = UIImage(data: png) {
                UIPasteboard.general.image = image
            }
        }
    }

    func notify(title: String, body: String) {
        let content = UNMutableNotificationContent()
        content.title = title
        content.body = body
        let req = UNNotificationRequest(
            identifier: UUID().uuidString,
            content: content,
            trigger: nil
        )
        UNUserNotificationCenter.current().add(req)
    }
}
