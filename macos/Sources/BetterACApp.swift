//  betterAC — a native SwiftUI launcher for Asheron's Call on macOS.
//
//  The mirror of the Linux GTK app: same flow, same shared Rust core underneath.
//  On launch it asks the core whether the game is set up (`ac_detect`); if not it
//  runs first-run setup with a progress view, then drops into the launcher.

import SwiftUI

@main
struct BetterACApp: App {
    var body: some Scene {
        WindowGroup("Asheron's Call") {
            RootView()
                .frame(minWidth: 760, minHeight: 520)
                .tint(.acAccent)
        }
        .windowResizability(.contentSize)
    }
}
