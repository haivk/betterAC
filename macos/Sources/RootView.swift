//  Routes between first-run setup and the launcher, based on `ac_detect`.

import SwiftUI

struct RootView: View {
    /// nil while the first detection is in flight.
    @State private var detected: DetectResult?

    var body: some View {
        Group {
            switch detected {
            case .none:
                ProgressView("Checking your installation…")
                    .controlSize(.large)
            case .some(let d) where d.ready:
                // A reset deletes the install, so re-detecting is what swaps this
                // for the setup screen.
                LauncherView(onReset: redetect)
            case .some:
                SetupView(onComplete: redetect)
            }
        }
        .task { redetect() }
    }

    /// Re-run detection off the main thread and swap the view when it returns.
    private func redetect() {
        detected = nil
        Task.detached {
            let result = ACCore.detect()
            await MainActor.run { detected = result }
        }
    }
}
