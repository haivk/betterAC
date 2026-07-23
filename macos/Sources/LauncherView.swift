//  The launcher proper: pick a server, enter the account for it, press Play.
//
//  Servers with a saved login are pinned to the top in their own "Saved" section
//  and tinted with the app accent. Credentials are persisted per-server through
//  the same config the Linux app writes (`ac_config_get`/`ac_config_set`).
//
//  The credential editor lives in a child `ServerDetailView` keyed by `.id`, not
//  in this view synced via onChange: each selected server gets a fresh editor
//  initialised from its saved entry, so switching servers can never show another
//  server's credentials.

import SwiftUI

struct LauncherView: View {
    /// Called after a reset wipes the install, so the root can route back to
    /// setup. Defaulted so previews and any future call site stay cheap.
    var onReset: () -> Void = {}

    @State private var servers: [Server] = []
    @State private var config = Config()
    @State private var selection: Server.ID?
    @State private var loading = true
    @State private var showingSettings = false

    /// Servers with a saved login, in the directory's existing order.
    private var savedServers: [Server] { servers.filter { config.entry(id: $0.id) != nil } }
    private var otherServers: [Server] { servers.filter { config.entry(id: $0.id) == nil } }
    private var selectedServer: Server? { servers.first { $0.id == selection } }

    var body: some View {
        NavigationSplitView {
            sidebar
                .navigationSplitViewColumnWidth(min: 240, ideal: 280)
        } detail: {
            if let server = selectedServer {
                // .id ties the editor's identity to the server, so selecting a
                // different one rebuilds it with that server's saved credentials.
                ServerDetailView(server: server, config: $config)
                    .id(server.id)
            } else {
                ContentUnavailableViewCompat(
                    title: "Choose a server",
                    systemImage: "list.bullet",
                    description: "Pick a server on the left to enter your account and play."
                )
            }
        }
        .task { await load() }
        .toolbar {
            ToolbarItem(placement: .primaryAction) {
                Button {
                    showingSettings = true
                } label: {
                    Image(systemName: "gearshape")
                }
                .help("Settings")
                .accessibilityLabel("Settings")
            }
        }
        .sheet(isPresented: $showingSettings) {
            SettingsView(onReset: onReset)
        }
    }

    // MARK: sidebar — the directory, saved servers pinned on top

    private var sidebar: some View {
        Group {
            if loading {
                ProgressView().controlSize(.small)
            } else {
                List(selection: $selection) {
                    if !savedServers.isEmpty {
                        Section("Saved") {
                            ForEach(savedServers) { row($0, saved: true).tag($0.id) }
                        }
                    }
                    Section(savedServers.isEmpty ? "Servers" : "All Servers") {
                        ForEach(otherServers) { row($0, saved: false).tag($0.id) }
                    }
                }
            }
        }
        .navigationTitle("Asheron's Call")
    }

    private func row(_ server: Server, saved: Bool) -> some View {
        HStack {
            VStack(alignment: .leading, spacing: 2) {
                Text(server.name)
                    .foregroundStyle(saved ? Color.acAccent : Color.primary)
                HStack(spacing: 6) {
                    Text(server.software.label)
                    if !server.ruleset.isEmpty { Text("· \(server.ruleset)") }
                }
                .font(.caption)
                .foregroundStyle(.secondary)
            }
            Spacer()
            HStack(spacing: 8) {
                if let online = server.online {
                    Text("\(online)")
                        .font(.caption.monospacedDigit())
                        .foregroundStyle(.secondary)
                }
                if saved {
                    Image(systemName: "person.crop.circle.badge.checkmark")
                        .foregroundStyle(Color.acAccent)
                }
            }
        }
    }

    // MARK: data

    private func load() async {
        let cfg = ACCore.loadConfig()
        let list = await Task.detached { ACCore.servers() }.value
        await MainActor.run {
            config = cfg
            servers = list
            loading = false
            // Reselect the last-played server if it is still in the directory.
            if selection == nil, let last = cfg.last, list.contains(where: { $0.id == last }) {
                selection = last
            }
        }
    }
}

/// Credentials + Play for one server. Owns its own account/password state,
/// initialised from the saved entry, so it is always in sync with the server it
/// was created for (see the `.id` on the call site).
struct ServerDetailView: View {
    let server: Server
    @Binding var config: Config

    @State private var account: String
    @State private var password: String
    @State private var launching = false
    @State private var launchError: String?

    init(server: Server, config: Binding<Config>) {
        self.server = server
        self._config = config
        let entry = config.wrappedValue.entry(id: server.id)
        _account = State(initialValue: entry?.account ?? "")
        _password = State(initialValue: entry?.password ?? "")
    }

    var body: some View {
        Form {
            Section {
                LabeledContent("Server", value: server.name)
                LabeledContent("Address", value: server.address)
                LabeledContent("Software", value: server.software.label)
            }
            Section("Account") {
                TextField("Account", text: $account)
                    .textContentType(.username)
                SecureField("Password", text: $password)
            }
            if let launchError {
                Label(launchError, systemImage: "exclamationmark.triangle.fill")
                    .foregroundStyle(.red)
            }
            Section {
                Button {
                    play()
                } label: {
                    if launching {
                        ProgressView().controlSize(.small)
                    } else {
                        Text("Play").frame(maxWidth: .infinity)
                    }
                }
                .buttonStyle(.borderedProminent)
                .controlSize(.large)
                .disabled(launching || account.isEmpty || password.isEmpty)
            }
        }
        .formStyle(.grouped)
        .navigationTitle(server.name)
    }

    private func play() {
        launchError = nil
        launching = true

        // Persist the account for this server (this also moves it into the pinned
        // "Saved" section, via the config binding) before launching.
        var entry = config.entry(id: server.id) ?? Entry.from(server)
        entry.account = account
        entry.password = password
        config.upsert(entry)
        config.last = server.id
        if let saveError = ACCore.saveConfig(config) {
            launchError = saveError
            launching = false
            return
        }

        let (server, account, password) = (server, account, password)
        Task.detached {
            let error = ACCore.launch(server: server, account: account, password: password)
            await MainActor.run {
                launching = false
                launchError = error
            }
        }
    }
}

/// `ContentUnavailableView` is macOS 14+, but the deployment target is 13. This
/// is the same empty-state look on both.
struct ContentUnavailableViewCompat: View {
    let title: String
    let systemImage: String
    let description: String

    var body: some View {
        VStack(spacing: 10) {
            Image(systemName: systemImage)
                .font(.system(size: 40))
                .foregroundStyle(.secondary)
            Text(title).font(.title3.bold())
            Text(description)
                .font(.callout)
                .foregroundStyle(.secondary)
                .multilineTextAlignment(.center)
                .frame(maxWidth: 320)
        }
        .padding()
    }
}
