import JCodeKit
import SwiftUI

/// Top-level router: pairing when no server, chat otherwise.
struct RootView: View {
    @Environment(AppModel.self) private var model
    @State private var deepLinkError: String?

    var body: some View {
        ZStack {
            Theme.background.ignoresSafeArea()
            if model.activeServer == nil {
                PairingView()
            } else {
                ChatView()
            }
        }
        .task {
            // Auto-connect to the most recent server on launch.
            if let server = model.activeServer, !model.isConnected {
                model.connect(to: server)
            }
        }
        .onOpenURL { url in
            guard let payload = PairURI.parse(url.absoluteString) else { return }
            Task {
                do {
                    try await model.pair(
                        gateway: payload.gateway,
                        code: payload.code,
                        deviceName: UIDevice.current.name
                    )
                } catch {
                    deepLinkError = "Pairing failed: \(error.localizedDescription)"
                }
            }
        }
        .alert("Pairing", isPresented: .init(
            get: { deepLinkError != nil },
            set: { if !$0 { deepLinkError = nil } }
        )) {
            Button("OK", role: .cancel) {}
        } message: {
            Text(deepLinkError ?? "")
        }
    }
}

/// Connection status pill shown in the chat header.
struct StatusPill: View {
    let phase: ConnectionPhase

    var body: some View {
        HStack(spacing: 6) {
            Circle()
                .fill(color)
                .frame(width: 8, height: 8)
            Text(label)
                .font(Theme.mono(12))
                .foregroundStyle(Theme.textSecondary)
        }
        .padding(.horizontal, 10)
        .padding(.vertical, 5)
        .background(Theme.surface)
        .clipShape(Capsule())
        .overlay(Capsule().stroke(Theme.border, lineWidth: 1))
    }

    private var color: Color {
        switch phase {
        case .connected: Theme.mint
        case .connecting, .reconnecting: Theme.warning
        case .disconnected, .failed: Theme.error
        }
    }

    private var label: String {
        switch phase {
        case .connected: "live"
        case .connecting: "connecting"
        case .reconnecting(let attempt): "retry \(attempt)"
        case .disconnected: "offline"
        case .failed: "failed"
        }
    }
}

/// Dismissible error banner.
struct ErrorBanner: View {
    let message: String
    let dismiss: () -> Void

    var body: some View {
        HStack(spacing: 10) {
            Image(systemName: "exclamationmark.triangle.fill")
                .foregroundStyle(Theme.error)
            Text(message)
                .font(.footnote)
                .foregroundStyle(Theme.textPrimary)
                .lineLimit(3)
            Spacer(minLength: 0)
            Button(action: dismiss) {
                Image(systemName: "xmark")
                    .font(.caption.weight(.semibold))
                    .foregroundStyle(Theme.textSecondary)
            }
        }
        .padding(12)
        .background(Theme.error.opacity(0.12))
        .clipShape(RoundedRectangle(cornerRadius: 12))
        .overlay(
            RoundedRectangle(cornerRadius: 12)
                .stroke(Theme.error.opacity(0.35), lineWidth: 1)
        )
        .padding(.horizontal)
    }
}
