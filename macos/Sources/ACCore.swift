//  The Swift face of the ac-core C ABI.
//
//  Every string the C side returns is owned by us and must go back through
//  `ac_string_free`; `take` centralises that so no caller has to remember. The
//  networking/disk calls here (`servers`, `detect`, `loadConfig`, `launch`) block,
//  so callers run them off the main thread.

import Foundation

enum ACCore {
    private static let decoder = JSONDecoder()
    private static let encoder = JSONEncoder()

    /// Consume a C string returned by the ABI: copy it to a Swift `String`, then
    /// free the original. Null becomes nil.
    private static func take(_ ptr: UnsafeMutablePointer<CChar>?) -> String? {
        guard let ptr else { return nil }
        defer { ac_string_free(ptr) }
        return String(cString: ptr)
    }

    private static func decode<T: Decodable>(_ type: T.Type, from json: String?) -> T? {
        guard let json, let data = json.data(using: .utf8) else { return nil }
        return try? decoder.decode(type, from: data)
    }

    /// ac-core version. Static storage on the C side — do NOT free.
    static var coreVersion: String {
        guard let p = ac_core_version() else { return "?" }
        return String(cString: p)
    }

    /// The server directory. Live from treestats, else the bundled snapshot.
    /// Blocks on the network; call off the main thread.
    static func servers() -> [Server] {
        decode([Server].self, from: take(ac_servers_json())) ?? []
    }

    static func loadConfig() -> Config {
        decode(Config.self, from: take(ac_config_get())) ?? Config()
    }

    /// Persist config. Returns nil on success, or an error string.
    @discardableResult
    static func saveConfig(_ config: Config) -> String? {
        guard let data = try? encoder.encode(config),
              let json = String(data: data, encoding: .utf8)
        else { return "could not encode config" }
        return json.withCString { take(ac_config_set($0)) }
    }

    // --- Decal ---------------------------------------------------------------
    //
    // Which plugins are enabled is not part of Config: it lives in the prefix
    // registry, because that is what Decal reads. So these query the prefix
    // directly and the settings UI refreshes from them rather than caching.

    /// Is Decal provisioned in the prefix? False means either it is switched off,
    /// or it is on but setup has not run since — the UI distinguishes those.
    static var decalInstalled: Bool {
        take(ac_decal_installed()) == "1"
    }

    /// Every plugin Decal knows about. Empty when Decal is not installed.
    static func decalPlugins() -> [DecalPlugin] {
        decode([DecalPlugin].self, from: take(ac_decal_plugins_json())) ?? []
    }

    /// Both of the above in one trip, so a caller that needs the plugin list pays
    /// for one hop off the main thread rather than two. Reading the plugins means
    /// spawning `reg query` inside the prefix, which on a cold wineserver takes
    /// seconds — never call this on the main thread.
    static func decalState() -> (installed: Bool, plugins: [DecalPlugin]) {
        let installed = decalInstalled
        return (installed, installed ? decalPlugins() : [])
    }

    /// Turn one plugin on or off. Returns nil on success, or an error string.
    @discardableResult
    static func setDecalPlugin(_ clsid: String, enabled: Bool) -> String? {
        clsid.withCString { take(ac_decal_set_plugin($0, enabled)) }
    }

    /// Open Decal's own configuration UI. Returns nil on success, or an error.
    ///
    /// The agent it starts has no window — it appears in the menu bar and opens its
    /// dialog when clicked — so this returns as soon as the process is spawned, and
    /// the caller must tell the user where to look.
    @discardableResult
    static func openDecalSettings() -> String? { take(ac_decal_open_settings()) }

    /// What is running, what is available, and who owns updating this copy.
    ///
    /// Makes a network request — never call it on the main thread.
    static func updateStatus() -> UpdateStatus? {
        guard let json = take(ac_update_status()) else { return nil }
        return decode(UpdateStatus.self, from: json)
    }

    /// Download and install the newest release. Blocking and slow; off the main
    /// thread only.
    static func installUpdate() -> UpdateResult? {
        guard let json = take(ac_update_install()) else { return nil }
        return decode(UpdateResult.self, from: json)
    }

    /// Shut the prefix down. Decal's agent outlives the settings sheet on purpose,
    /// so something has to end it or its menu-bar icon is left behind.
    static func shutdownPrefix() { ac_decal_shutdown() }

    /// Run a plugin's installer (`.msi` or `.exe`) inside the prefix. nil on
    /// success, else an error string.
    ///
    /// The installer shows its own UI, so this does not return until the user has
    /// dismissed it — never call it on the main thread.
    @discardableResult
    static func installPlugin(path: String) -> String? {
        path.withCString { take(ac_decal_install_plugin($0)) }
    }

    /// Register a plugin from a DLL on disk, disabled. nil on success.
    @discardableResult
    static func addDecalPlugin(path: String) -> String? {
        path.withCString { take(ac_decal_add_plugin($0)) }
    }

    /// Whether the game is installed and ready to launch.
    static func detect() -> DetectResult {
        decode(DetectResult.self, from: take(ac_detect()))
            ?? DetectResult(ready: false, ac_dir: nil, error: "detection failed")
    }

    /// Kick off first-run setup on a background thread. After a stopped or failed
    /// run this resumes it — the steps skip whatever already succeeded.
    static func startSetup() { ac_setup_start() }

    /// Ask a running setup to stop. It ends at the next cancellation point, which
    /// is immediate mid-download and otherwise as soon as the current external
    /// command returns.
    static func cancelSetup() { ac_setup_cancel() }

    /// A snapshot of the whole setup run: every step, with its own state and
    /// progress. Valid before setup starts too, where it is the plan.
    static func pollSetup() -> SetupRun {
        decode(SetupRun.self, from: take(ac_setup_poll())) ?? SetupRun()
    }

    /// What a reset would delete, for the confirmation list. Cheap; reads config.
    static func resetTargets() -> [ResetTarget] {
        decode([ResetTarget].self, from: take(ac_reset_targets_json())) ?? []
    }

    /// Delete the prefix, engine and settings. Returns nil on success, or an
    /// error string. Touches a lot of disk — call off the main thread.
    static func reset() -> String? { take(ac_reset()) }

    /// Launch the client. Returns nil on a successful spawn, or an error string.
    static func launch(server: Server, account: String, password: String) -> String? {
        guard let data = try? encoder.encode(server),
              let json = String(data: data, encoding: .utf8)
        else { return "could not encode server" }
        return json.withCString { s in
            account.withCString { a in
                password.withCString { p in take(ac_launch(s, a, p)) }
            }
        }
    }
}
