//  Swift mirrors of the ac-core types that cross the FFI as JSON.
//
//  These must stay in step with the serde shapes on the Rust side:
//   * `Software` serialises as the bare variant name ("Ace" / "Gdle").
//   * `Server.ruleset` is carried under the JSON key "type".
//  Everything else is a straight field-name match.

import Foundation

enum Software: String, Codable {
    case ace = "Ace"
    case gdle = "Gdle"

    var label: String { self == .ace ? "ACE" : "GDLE" }
}

struct Players: Codable, Hashable {
    let count: Int
    var age: String = ""
}

/// One entry in the live server directory (from `ac_servers_json`).
struct Server: Codable, Identifiable, Hashable {
    var name: String
    var description: String = ""
    var ruleset: String = ""
    var software: Software = .ace
    var host: String
    var port: String
    var players: Players?
    var website_url: String?
    var discord_url: String?

    var id: String { "\(host):\(port)" }
    var address: String { "\(host):\(port)" }

    /// Live population, or nil when the server doesn't report a fresh count. The
    /// Rust side already sorts busiest-first and only emits fresh counts here, so
    /// this is just for display.
    var online: Int? { players?.count }

    enum CodingKeys: String, CodingKey {
        case name, description
        case ruleset = "type"
        case software, host, port, players, website_url, discord_url
    }
}

/// A saved server + the account played on it. Mirrors `config::Entry`.
struct Entry: Codable, Identifiable, Hashable {
    var name: String
    var host: String
    var port: String
    var software: Software = .ace
    var ruleset: String = ""
    var account: String = ""
    var password: String = ""

    var id: String { "\(host):\(port)" }

    /// The `Server` shape `ac_launch` expects. Only software/host/port actually
    /// drive the command line, but we send a full object so it decodes cleanly.
    func toServer() -> Server {
        Server(name: name, ruleset: ruleset, software: software, host: host, port: port)
    }

    static func from(_ s: Server) -> Entry {
        Entry(name: s.name, host: s.host, port: s.port, software: s.software, ruleset: s.ruleset)
    }
}

/// Decal, the plugin framework. Mirrors `config::DecalConfig`.
///
/// Only the master switch lives here. Which *plugins* are on is kept in the Wine
/// prefix's registry, because that is what Decal itself reads — see `decalPlugins`.
struct DecalConfig: Codable {
    var enabled: Bool = false
}

/// One plugin Decal knows about. Mirrors `decal::DecalPlugin`.
struct DecalPlugin: Codable, Identifiable {
    let clsid: String
    let name: String
    var enabled: Bool

    var id: String { clsid }
}

/// Persisted state. Mirrors `config::Config`.
struct Config: Codable {
    var prefix: String = ""
    var servers: [Entry] = []
    var last: String?
    var decal: DecalConfig = DecalConfig()

    func entry(id: String) -> Entry? { servers.first { $0.id == id } }

    /// Insert or update the entry for this server id, preserving nothing else.
    mutating func upsert(_ e: Entry) {
        if let i = servers.firstIndex(where: { $0.id == e.id }) {
            servers[i] = e
        } else {
            servers.append(e)
        }
    }
}

/// One thing `ac_reset` will delete. Mirrors `reset::Target`.
///
/// The paths come from the Rust side rather than being rebuilt here, so the
/// warning always names what will actually be removed.
struct ResetTarget: Codable, Identifiable {
    let label: String
    let path: String

    var id: String { path }
}

/// Result of `ac_detect`.
struct DetectResult: Codable {
    let ready: Bool
    let ac_dir: String?
    let error: String?
}

/// Where one setup step is in its life. Mirrors `setup::StepState`.
enum StepState: String, Codable {
    case pending, running, done, skipped, failed

    /// Nothing left to do for this step, either way.
    var isFinished: Bool { self == .done || self == .skipped }
}

/// One row of the setup checklist. The Rust side sends the label and detail along
/// with the state, so this side never keeps its own copy of the step vocabulary —
/// it renders whatever it is handed. Mirrors `setup::StepStatus`.
struct SetupStepStatus: Codable, Identifiable {
    var step: String
    var label: String
    var detail: String
    var state: StepState
    var fraction: Double
    var message: String

    var id: String { step }
}

/// One `ac_setup_poll` snapshot: the whole run, every step. Mirrors
/// `setup::RunState`. Polling before setup starts returns the plan — every step
/// pending — which is what the pre-flight list shows.
struct SetupRun: Codable {
    var started: Bool = false
    var done: Bool = false
    /// Stopped because the user asked, not because anything broke.
    var cancelled: Bool = false
    var error: String?
    var steps: [SetupStepStatus] = []

    /// Steps that are behind us, for the "step 4 of 10" line.
    var completed: Int { steps.filter { $0.state.isFinished }.count }

    /// The step being worked on right now, if any.
    var current: SetupStepStatus? { steps.first { $0.state == .running } }

    /// What is still ahead: the active step first, then what is queued behind it.
    /// Finished steps drop out, so the list shortens as setup proceeds and the
    /// thing happening now is always the row at the top.
    var remaining: [SetupStepStatus] { steps.filter { !$0.state.isFinished } }
}

/// A release newer than this build — mirrors `ac_core::update::Release`.
struct UpdateRelease: Codable {
    var version: String
    var asset: String
    var sha256: String
}

/// Mirrors `ac_core::update::Status`, plus the `error` key the FFI substitutes
/// when the check could not be made at all.
struct UpdateStatus: Codable {
    var current: String
    var available: UpdateRelease?
    /// `"homebrew"` when this copy must be updated with `brew upgrade` rather than
    /// by replacing it ourselves. Absent when the check failed.
    var source: String?
    var error: String?
}

/// The outcome of `ac_update_install`.
struct UpdateResult: Codable {
    /// The app must exit *now* — a helper is waiting to swap the bundle, and it
    /// cannot proceed while this process holds it open.
    var quit: Bool?
    var uptodate: Bool?
    var error: String?
}
