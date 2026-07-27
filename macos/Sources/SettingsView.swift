//  Settings, reached from the gear in the launcher's toolbar.
//
//  Two things live here. The escape hatch that undoes an install: destructive and
//  irreversible, so it is deliberately three steps from idle — open settings, press
//  the button, confirm — and the confirmation names the real directories, read from
//  `ac_reset_targets_json` rather than described in hardcoded prose that could
//  drift from what actually gets deleted.
//
//  And Decal: its plugins, a way to add more, and a hand-off to DenAgent for the
//  parts Decal only exposes itself. Every bit of that state is registry data that
//  only a Wine process can answer for, so it is both slow to read — hence the
//  spinner, while the rest of the sheet stays live — and shared with an editor we
//  do not control, hence the re-read whenever the app comes back to the front.
//
//  Deployment target is macOS 13, so nothing here uses 14+ API.

import AppKit
import SwiftUI
import UniformTypeIdentifiers

struct SettingsView: View {
    /// Called after a successful reset. The root re-runs detection, which now
    /// fails, which routes back to the setup screen.
    let onReset: () -> Void

    @Environment(\.dismiss) private var dismiss

    @State private var targets: [ResetTarget] = []
    @State private var confirming = false
    @State private var resetting = false
    @State private var error: String?

    @State private var decalEnabled = false
    @State private var decalInstalled = false
    @State private var plugins: [DecalPlugin] = []
    @State private var decalError: String?
    /// Nil until the first read of the prefix finishes. Reading it costs a Wine
    /// process spawn (seconds on a cold wineserver), so the section renders a
    /// placeholder rather than making the whole sheet wait on it.
    @State private var decalLoaded = false
    @State private var agentOpened = false
    @State private var installing = false
    @State private var registeredDisabled = false
    /// CLSIDs with a write in flight. Writing means a Wine process spawn, so a
    /// toggle is held disabled until its write lands rather than letting a second
    /// flip race the first.
    @State private var writing: Set<String> = []

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
                    // Decal is decided at install time (settings is unreachable
                    // then), so this only *manages* it. The switches write the same
                    // per-user key Decal's own agent writes, so the two agree; the
                    // list is re-read on reactivation to stay in step when the user
                    // has been changing things over there.
                    if !decalLoaded {
                        HStack(spacing: 8) {
                            ProgressView().controlSize(.small)
                            Text("Reading Decal's state from the prefix…")
                                .font(.callout)
                                .foregroundStyle(.secondary)
                        }
                    } else if decalInstalled {
                        Toggle("Enable Decal", isOn: $decalEnabled)
                            .onChange(of: decalEnabled) { on in
                                var config = ACCore.loadConfig()
                                config.decal.enabled = on
                                decalError = ACCore.saveConfig(config)
                            }

                        if plugins.isEmpty {
                            Text("No plugins are registered.")
                                .font(.caption)
                                .foregroundStyle(.secondary)
                        } else {
                            // An explicit binding rather than `$plugins` + onChange:
                            // re-reading the registry replaces this array, and an
                            // onChange would read that as the user flipping every
                            // switch that had moved and write it straight back.
                            // A setter only runs when the switch is actually clicked.
                            ForEach(plugins) { plugin in
                                Toggle(plugin.name.isEmpty ? plugin.clsid : plugin.name,
                                       isOn: binding(for: plugin))
                                    .disabled(writing.contains(plugin.clsid))
                            }
                        }

                        HStack(spacing: 8) {
                            Button("Install Plugin…") { installPlugin() }
                                .disabled(installing)
                            Button("Open Decal Settings…") { openAgent() }

                            if installing {
                                ProgressView().controlSize(.small)
                                Text("Installer is running…")
                                    .font(.caption)
                                    .foregroundStyle(.secondary)
                            }
                        }

                        if registeredDisabled {
                            Label(
                                "Registered, switched off. Turn it on in Decal's settings.",
                                systemImage: "checkmark.circle"
                            )
                            .font(.caption)
                            .foregroundStyle(.secondary)
                        }

                        if agentOpened {
                            Label(
                                "Decal's agent has no window — click its icon in the menu bar to configure plugins.",
                                systemImage: "menubar.arrow.up.rectangle"
                            )
                            .font(.caption)
                            .foregroundStyle(.secondary)
                        }
                    } else {
                        Text("Decal was not installed. Reset and set up again to add it.")
                            .font(.callout)
                            .foregroundStyle(.secondary)
                    }

                    if let decalError {
                        Label(decalError, systemImage: "exclamationmark.triangle.fill")
                            .font(.caption)
                            .foregroundStyle(.red)
                    }
                } header: {
                    Text("Decal")
                } footer: {
                    Text("Decal is a third-party plugin framework for Asheron's Call. Changes apply the next time the game starts.")
                        .font(.caption)
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
        .task {
            // Cheap (config + disk paths), so the sheet is usable immediately.
            targets = ACCore.resetTargets()
            decalEnabled = ACCore.loadConfig().decal.enabled
            await refreshDecal()
        }
        // Coming back from Decal's agent is exactly when the list is most likely to
        // be wrong — the user went there to change something. Re-reading on
        // reactivation is what keeps a read-only list honest without a Refresh
        // button; it is silent, so the section does not flash a spinner each time
        // the user tabs away and back.
        .onReceive(NotificationCenter.default.publisher(
            for: NSApplication.didBecomeActiveNotification
        )) { _ in
            guard decalLoaded, !installing, writing.isEmpty else { return }
            Task { await refreshDecal() }
        }
        .alert("Reset the installation?", isPresented: $confirming) {
            Button("Cancel", role: .cancel) {}
            Button("Reset", role: .destructive) { performReset() }
        } message: {
            Text("This deletes the installed game, the Windows prefix and your saved servers and passwords. It cannot be undone.")
        }
    }

    /// Re-read Decal's state from the prefix. The registry is the source of truth
    /// for which plugins are on, so the list is always read back rather than
    /// assumed.
    ///
    /// Off the main thread, always: this spawns `reg query` inside the prefix and a
    /// cold wineserver makes that a 5–10 second call. Doing it inline is what used
    /// to freeze the whole settings sheet until Decal had answered.
    private func refreshDecal() async {
        let state = await Task.detached { ACCore.decalState() }.value
        decalInstalled = state.installed
        plugins = state.plugins
        decalLoaded = true
    }

    /// The switch for one plugin: reads through to the current list, and on being
    /// set moves the UI immediately and starts the write.
    private func binding(for plugin: DecalPlugin) -> Binding<Bool> {
        let clsid = plugin.clsid
        let fallback = plugin.enabled
        return Binding<Bool>(
            get: { () -> Bool in
                guard let current = plugins.first(where: { $0.clsid == clsid }) else {
                    return fallback
                }
                return current.enabled
            },
            set: { (on: Bool) in
                if let i = plugins.firstIndex(where: { $0.clsid == clsid }) {
                    plugins[i].enabled = on
                }
                setPlugin(clsid, enabled: on)
            }
        )
    }

    /// Switch one plugin on or off.
    ///
    /// The toggle has already moved when this runs — the write is what follows the
    /// UI, not the other way round, because it costs a Wine process spawn and a
    /// switch that lags a click by a second feels broken. If the write fails the
    /// list is re-read, which snaps the toggle back to whatever the registry
    /// actually says rather than leaving the UI asserting something untrue.
    ///
    /// Decal reads this at client startup, so flipping one mid-session does not
    /// affect a game already running.
    private func setPlugin(_ clsid: String, enabled: Bool) {
        writing.insert(clsid)
        Task {
            let failure = await Task.detached {
                ACCore.setDecalPlugin(clsid, enabled: enabled)
            }.value
            writing.remove(clsid)
            decalError = failure
            if failure != nil { await refreshDecal() }
        }
    }

    /// Add a plugin, by whichever of the two routes the file the user picked calls
    /// for.
    ///
    /// The **downloaded zip** is the best thing to hand this, and what plugins are
    /// actually distributed as: the archive already says which files belong
    /// together, so it is unpacked whole and the installer inside runs with its
    /// libraries beside it. A loose `.msi`/`.exe` works too, but then the
    /// dependencies sitting next to it have to be inferred, which is guesswork the
    /// archive makes unnecessary.
    ///
    /// A bare **DLL** is the fallback for plugins distributed without an installer —
    /// `addDecalPlugin` reads the CLSID and class name straight out of the
    /// assembly's metadata and writes Decal's registration itself, so it needs
    /// neither RegAsm nor a .NET install. That only works for *managed* plugins,
    /// which is why it is the fallback and not the front door.
    ///
    /// The panel is closed before either runs: an installer puts its own windows on
    /// screen, and stacking those behind an open file dialog is a good way to lose
    /// them. The plugin list is re-read afterwards rather than guessed at, because
    /// what a third-party installer registered is its business, not ours.
    private func installPlugin() {
        let panel = NSOpenPanel()
        panel.title = "Choose a Decal plugin"
        panel.message =
            "Pick the plugin's downloaded .zip, or its .msi/.exe installer — or its .dll if it has none."
        panel.allowedContentTypes = ["zip", "msi", "exe", "dll"]
            .compactMap { UTType(filenameExtension: $0) }
        panel.allowsMultipleSelection = false
        panel.canChooseDirectories = false
        guard panel.runModal() == .OK, let url = panel.url else { return }

        let isDLL = url.pathExtension.lowercased() == "dll"
        decalError = nil
        installing = true
        Task {
            let failure = await Task.detached {
                isDLL
                    ? ACCore.addDecalPlugin(path: url.path)
                    : ACCore.installPlugin(path: url.path)
            }.value
            decalError = failure
            installing = false
            // A DLL is registered switched off, by Decal's own convention — say so,
            // otherwise it looks like the registration silently failed.
            registeredDisabled = failure == nil && isDLL
            await refreshDecal()
        }
    }

    /// Hand off to Decal's own configuration UI.
    ///
    /// It is deliberately left running when this sheet closes, so the dialog stays
    /// reachable; the app kills the prefix on quit instead (see `BetterACApp`),
    /// which is also what clears the menu-bar icon.
    private func openAgent() {
        decalError = ACCore.openDecalSettings()
        agentOpened = decalError == nil
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
