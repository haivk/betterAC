//  betterAC — a native SwiftUI launcher for Asheron's Call on macOS.
//
//  The mirror of the Linux GTK app: same flow, same shared Rust core underneath.
//  On launch it asks the core whether the game is set up (`ac_detect`); if not it
//  runs first-run setup with a progress view, then drops into the launcher.

import AppKit
import SwiftUI

@main
struct BetterACApp: App {
    @NSApplicationDelegateAdaptor(AppDelegate.self) private var delegate

    var body: some Scene {
        WindowGroup("Asheron's Call") {
            RootView()
                .frame(minWidth: 760, minHeight: 520)
                .tint(.acAccent)
        }
        .windowResizability(.contentSize)
    }
}

/// Exists for one reason: something has to end the Wine session we may have
/// started.
///
/// Opening Decal's settings leaves its agent running on purpose, so the dialog
/// stays reachable for as long as betterAC is up. That agent's menu-bar icon is
/// owned by the prefix's `explorer.exe` rather than by the agent itself, so it
/// survives the agent and nothing later clears it — quitting without tearing the
/// session down leaves a dead icon in the user's menu bar.
///
/// This is safe here because the app quitting means nothing of ours should still be
/// running. The game is the exception, and it isn't one: launching it hands off to a
/// process the user closes themselves, and betterAC is not normally quit mid-session.
final class AppDelegate: NSObject, NSApplicationDelegate {
    func applicationWillTerminate(_ notification: Notification) {
        ACCore.shutdownPrefix()
    }
}
