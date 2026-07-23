//  Settings, reached from the gear in the launcher's toolbar.
//
//  Right now this is one thing: the escape hatch that undoes an install. It is a
//  destructive, irreversible action, so it is deliberately three steps from idle
//  — open settings, press the button, confirm — and the confirmation names the
//  real directories, read from `ac_reset_targets_json` rather than described in
//  hardcoded prose that could drift from what actually gets deleted.
//
//  Deployment target is macOS 13, so nothing here uses 14+ API.

import SwiftUI

struct SettingsView: View {
    /// Called after a successful reset. The root re-runs detection, which now
    /// fails, which routes back to the setup screen.
    let onReset: () -> Void

    @Environment(\.dismiss) private var dismiss

    @State private var targets: [ResetTarget] = []
    @State private var confirming = false
    @State private var resetting = false
    @State private var error: String?

    private var appVersion: String {
        let info = Bundle.main.infoDictionary
        let short = info?["CFBundleShortVersionString"] as? String ?? "?"
        return short
    }

    var body: some View {
        VStack(spacing: 0) {
            Form {
                Section("About") {
                    LabeledContent("BetterAC", value: appVersion)
                    LabeledContent("Core", value: ACCore.coreVersion)
                }

                Section {
                    VStack(alignment: .leading, spacing: 10) {
                        Text("Delete the Windows prefix, the Wine engine and your saved servers, then run setup again from scratch.")
                            .font(.callout)

                        if !targets.isEmpty {
                            VStack(alignment: .leading, spacing: 4) {
                                ForEach(targets) { t in
                                    HStack(alignment: .firstTextBaseline, spacing: 6) {
                                        Text("•").foregroundStyle(.secondary)
                                        VStack(alignment: .leading, spacing: 1) {
                                            Text(t.label).font(.caption.bold())
                                            Text(t.path)
                                                .font(.caption2.monospaced())
                                                .foregroundStyle(.secondary)
                                                .textSelection(.enabled)
                                                .lineLimit(2)
                                                .truncationMode(.middle)
                                        }
                                    }
                                }
                            }
                            .padding(.vertical, 2)
                        }

                        Text("Downloaded installers are kept, so setting up again does not re-download them.")
                            .font(.caption)
                            .foregroundStyle(.secondary)

                        if let error {
                            Label(error, systemImage: "exclamationmark.triangle.fill")
                                .font(.caption)
                                .foregroundStyle(.red)
                        }

                        HStack(spacing: 8) {
                            Button(role: .destructive) {
                                confirming = true
                            } label: {
                                Text("Reset Installation…")
                            }
                            .disabled(resetting)

                            if resetting {
                                ProgressView().controlSize(.small)
                                Text("Removing…")
                                    .font(.caption)
                                    .foregroundStyle(.secondary)
                            }
                        }
                    }
                    .padding(.vertical, 4)
                } header: {
                    Text("Reset")
                }
            }
            .formStyle(.grouped)

            Divider()
            HStack {
                Spacer()
                Button("Done") { dismiss() }
                    .keyboardShortcut(.defaultAction)
            }
            .padding(12)
        }
        .frame(width: 520, height: 480)
        .task { targets = ACCore.resetTargets() }
        .alert("Reset the installation?", isPresented: $confirming) {
            Button("Cancel", role: .cancel) {}
            Button("Reset", role: .destructive) { performReset() }
        } message: {
            Text("This deletes the installed game, the Windows prefix and your saved servers and passwords. It cannot be undone.")
        }
    }

    private func performReset() {
        error = nil
        resetting = true
        Task.detached {
            let failure = ACCore.reset()
            await MainActor.run {
                resetting = false
                if let failure {
                    error = failure
                } else {
                    // Order matters: hand control back to the root before this
                    // sheet's host view is swapped out from under it.
                    dismiss()
                    onReset()
                }
            }
        }
    }
}
