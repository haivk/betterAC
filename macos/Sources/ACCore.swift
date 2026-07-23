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
