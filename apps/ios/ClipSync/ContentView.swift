import SwiftUI
import UIKit

private let mint = Color(red: 0.31, green: 0.89, blue: 0.76)
private let ink = Color(red: 0.07, green: 0.08, blue: 0.09)
private let panel = Color(red: 0.12, green: 0.13, blue: 0.15)

final class AppModel: ObservableObject {
    @Published var statusLine = "Starting…"
    @Published var pairingCode: String?
    @Published var joinCode = ""
    @Published var roomId: String?
    @Published var busy = false
    @Published var error: String?

    private var client: ClipSyncClient?
    private let clipboard = IOSClipboard()

    func bootstrap() {
        do {
            let docs = FileManager.default.urls(for: .documentDirectory, in: .userDomainMask)[0]
            let dir = docs.appendingPathComponent("clipsync", isDirectory: true)
            try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
            let name = UIDevice.current.name
            client = try ClipSyncClient(
                dataDir: dir.path,
                relayUrl: "https://clipsync.develicit.dev",
                os: "ios",
                deviceName: name
            )
            refresh()
            if roomId != nil {
                try startSync()
            }
        } catch {
            self.error = error.localizedDescription
            statusLine = "Failed to start"
        }
    }

    func refresh() {
        guard let client else { return }
        do {
            let s = try client.status()
            roomId = s.roomId
            statusLine = s.roomId.map { "Paired · \($0.prefix(8))…" } ?? "Not paired"
        } catch {
            self.error = error.localizedDescription
        }
    }

    func create() {
        guard let client else { return }
        busy = true
        error = nil
        DispatchQueue.global(qos: .userInitiated).async {
            do {
                let offer = try client.createRoom()
                DispatchQueue.main.async {
                    self.pairingCode = offer.pairingCode
                    self.statusLine = "Waiting for join…"
                    self.busy = false
                    self.pollPaired()
                }
            } catch {
                DispatchQueue.main.async {
                    self.error = error.localizedDescription
                    self.busy = false
                }
            }
        }
    }

    func join() {
        guard let client else { return }
        let code = joinCode.filter(\.isNumber)
        busy = true
        error = nil
        DispatchQueue.global(qos: .userInitiated).async {
            do {
                let room = try client.joinRoom(code: code)
                try client.startSync(clipboard: self.clipboard)
                DispatchQueue.main.async {
                    self.roomId = room
                    self.statusLine = "Paired"
                    self.busy = false
                }
            } catch {
                DispatchQueue.main.async {
                    self.error = error.localizedDescription
                    self.busy = false
                }
            }
        }
    }

    private func startSync() throws {
        guard let client else { return }
        try client.startSync(clipboard: clipboard)
    }

    private func pollPaired() {
        DispatchQueue.main.asyncAfter(deadline: .now() + 1.5) {
            self.refresh()
            if self.roomId != nil {
                try? self.startSync()
            } else if self.pairingCode != nil {
                self.pollPaired()
            }
        }
    }
}

struct ContentView: View {
    @StateObject private var model = AppModel()

    var body: some View {
        ZStack {
            ink.ignoresSafeArea()
            VStack(alignment: .leading, spacing: 28) {
                HStack {
                    VStack(alignment: .leading, spacing: 4) {
                        Text("CLIPSYNC")
                            .font(.system(size: 13, weight: .semibold, design: .monospaced))
                            .foregroundStyle(mint)
                            .tracking(3)
                        Text("Native clipboard")
                            .font(.system(size: 28, weight: .semibold))
                            .foregroundStyle(.white)
                    }
                    Spacer()
                    Circle()
                        .fill(model.roomId == nil ? Color.white.opacity(0.15) : mint)
                        .frame(width: 10, height: 10)
                }

                Text(model.statusLine)
                    .font(.system(size: 15, weight: .medium, design: .monospaced))
                    .foregroundStyle(.white.opacity(0.65))

                if let code = model.pairingCode, model.roomId == nil {
                    VStack(alignment: .leading, spacing: 8) {
                        Text("PAIRING CODE")
                            .font(.system(size: 11, weight: .semibold, design: .monospaced))
                            .foregroundStyle(mint)
                            .tracking(2)
                        Text(code)
                            .font(.system(size: 48, weight: .bold, design: .monospaced))
                            .foregroundStyle(.white)
                            .textSelection(.enabled)
                    }
                    .padding(24)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .background(panel)
                    .clipShape(RoundedRectangle(cornerRadius: 20, style: .continuous))
                }

                if model.roomId == nil {
                    HStack(spacing: 12) {
                        Button(action: model.create) {
                            Label("Create room", systemImage: "plus.square.on.square")
                                .frame(maxWidth: .infinity)
                        }
                        .buttonStyle(MintButton())
                        .disabled(model.busy)
                    }

                    HStack(spacing: 10) {
                        TextField("Join code", text: $model.joinCode)
                            .keyboardType(.numberPad)
                            .font(.system(size: 20, weight: .medium, design: .monospaced))
                            .padding(14)
                            .background(panel)
                            .clipShape(RoundedRectangle(cornerRadius: 14, style: .continuous))
                            .foregroundStyle(.white)
                        Button("Join") { model.join() }
                            .buttonStyle(MintButton())
                            .disabled(model.busy || model.joinCode.filter(\.isNumber).count < 6)
                    }
                } else {
                    Text("Copy on this phone or a paired computer. ClipSync writes incoming items to the pasteboard while the app is open.")
                        .font(.system(size: 15))
                        .foregroundStyle(.white.opacity(0.7))
                        .fixedSize(horizontal: false, vertical: true)
                }

                if let error = model.error {
                    Text(error)
                        .font(.system(size: 13, design: .monospaced))
                        .foregroundStyle(Color(red: 1, green: 0.45, blue: 0.4))
                }

                Spacer()
            }
            .padding(24)
        }
        .preferredColorScheme(.dark)
        .onAppear(perform: model.bootstrap)
    }
}

private struct MintButton: ButtonStyle {
    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .font(.system(size: 16, weight: .semibold))
            .foregroundStyle(ink)
            .padding(.horizontal, 16)
            .padding(.vertical, 14)
            .background(mint.opacity(configuration.isPressed ? 0.7 : 1))
            .clipShape(RoundedRectangle(cornerRadius: 14, style: .continuous))
    }
}
