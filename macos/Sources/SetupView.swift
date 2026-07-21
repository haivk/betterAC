//  First-run setup, as a queue of steps.
//
//  Setup pulls down ~1.4 GB in three separate files and then does five local
//  jobs. Behind a single progress bar that read as one bar filling and emptying
//  over and over, with no way to tell which pass you were watching. So the core
//  reports every step separately (`setup::RunState`) and this view draws the lot:
//  one row per step, each with its own bar.
//
//  The list is a *queue*, not a log: the step being worked on is always the top
//  row, what is queued behind it is below, and a step that finishes drops out and
//  lets the rest slide up. Before you start, the queue is the whole plan, so you
//  can see the three downloads and the installer coming.
//
//  `ac_setup_start` spawns the work on a Rust thread; we poll `ac_setup_poll` four
//  times a second for the full run state — the same stream the GTK step list
//  renders on Linux.

import SwiftUI

struct SetupView: View {
    var onComplete: () -> Void

    @State private var run = SetupRun()
    /// Set once the user commits, so the list is a plan before and a queue after.
    @State private var started = false
    /// A stop has been asked for but the run hasn't noticed yet — a download stops
    /// at once, but wineboot or the installer wizard has to return first.
    @State private var stopping = false

    var body: some View {
        VStack(spacing: 16) {
            header

            // The queue scrolls: ten steps do not fit a 520pt window, and before
            // the run starts all ten are showing.
            ScrollView {
                StepList(steps: visibleSteps)
                    .padding(.horizontal, 1)
            }
            .frame(maxWidth: 520, maxHeight: 320)

            if let error = run.error {
                Label(error, systemImage: "exclamationmark.triangle.fill")
                    .foregroundStyle(.red)
                    .font(.callout)
                    .frame(maxWidth: 500, alignment: .leading)
            }

            buttons
        }
        .padding(28)
        .animation(.easeInOut(duration: 0.2), value: visibleSteps.map(\.id))
        // Ask the core for the step list up front: before setup starts it answers
        // with every step pending, which is exactly the plan we want to show.
        .task { refresh() }
    }

    /// The plan before the run, the queue during it. Finished steps fall off the
    /// top; a failed one stays, because it is the thing you need to look at.
    private var visibleSteps: [SetupStepStatus] {
        started ? run.remaining : run.steps
    }

    private var header: some View {
        VStack(spacing: 6) {
            Image(systemName: "arrow.down.circle")
                .font(.system(size: 34))
                .foregroundStyle(.tint)

            Text("Set up Asheron's Call")
                .font(.title2.bold())

            Text(subtitle)
                .font(.callout)
                .foregroundStyle(.secondary)
                .multilineTextAlignment(.center)
                .frame(maxWidth: 460)
        }
    }

    /// What to say up top: the pitch before you commit, where you are once you have.
    private var subtitle: String {
        guard started else {
            return """
                First run downloads the Windows runtime and the game, then installs it. \
                It needs a few gigabytes of space, and the installer will ask you to \
                click through it near the end.
                """
        }
        if stopping && !run.done { return "Stopping after the current step…" }
        if run.cancelled { return "Stopped. Everything finished so far is kept." }
        let total = run.steps.count
        let at = min(run.completed + 1, total)
        return "Step \(at) of \(total)"
    }

    @ViewBuilder
    private var buttons: some View {
        HStack(spacing: 12) {
            if isRunning {
                Button(stopping ? "Stopping…" : "Cancel") { stop() }
                    .buttonStyle(.bordered)
                    .controlSize(.large)
                    .disabled(stopping)
            } else {
                Button(startLabel) { start() }
                    .buttonStyle(.borderedProminent)
                    .controlSize(.large)
            }
        }
    }

    /// Setup is in flight: started, and the core hasn't reported it finished.
    private var isRunning: Bool { started && !run.done }

    /// The same button reads differently depending on what happened last — it
    /// always resumes, but "Try again" and "Resume" say something "Set up" doesn't.
    private var startLabel: String {
        if run.error != nil { return "Try again" }
        if run.cancelled { return "Resume setup" }
        return "Set up Asheron's Call"
    }

    private func start() {
        started = true
        stopping = false
        ACCore.startSetup()
        poll()
    }

    private func stop() {
        stopping = true
        ACCore.cancelSetup()
    }

    /// One snapshot, for the pre-start plan.
    private func refresh() {
        Task.detached {
            let snapshot = ACCore.pollSetup()
            await MainActor.run { run = snapshot }
        }
    }

    /// Poll until the run reports done, then either leave the outcome on screen or
    /// hand back to RootView to re-detect and show the launcher.
    private func poll() {
        Task {
            while true {
                let snapshot = await Task.detached { ACCore.pollSetup() }.value
                await MainActor.run {
                    run = snapshot
                    if snapshot.done { stopping = false }
                }
                if snapshot.done {
                    if snapshot.error == nil && !snapshot.cancelled {
                        await MainActor.run { onComplete() }
                    }
                    return
                }
                try? await Task.sleep(nanoseconds: 250_000_000)
            }
        }
    }
}

/// The queue itself: every step still to do, in order, each with its own bar.
private struct StepList: View {
    let steps: [SetupStepStatus]

    var body: some View {
        VStack(spacing: 0) {
            ForEach(Array(steps.enumerated()), id: \.element.id) { index, step in
                if index > 0 {
                    Divider().padding(.leading, 30)
                }
                StepRow(step: step)
                    .padding(.vertical, 9)
                    .padding(.horizontal, 12)
                    .transition(.opacity.combined(with: .move(edge: .top)))
            }
        }
        .background(.quaternary.opacity(0.4), in: RoundedRectangle(cornerRadius: 10))
    }
}

private struct StepRow: View {
    let step: SetupStepStatus

    var body: some View {
        HStack(alignment: .top, spacing: 10) {
            StepIcon(state: step.state)
                .frame(width: 16, height: 16)
                .padding(.top, 1)

            VStack(alignment: .leading, spacing: 4) {
                HStack(alignment: .firstTextBaseline) {
                    Text(step.label)
                        .font(.callout.weight(step.state == .running ? .semibold : .regular))
                        .foregroundStyle(step.state == .pending ? .secondary : .primary)
                    Spacer(minLength: 8)
                    if let trailing {
                        Text(trailing)
                            .font(.caption.monospacedDigit())
                            .foregroundStyle(.secondary)
                    }
                }

                Text(step.message)
                    .font(.caption)
                    .foregroundStyle(step.state == .failed ? .red : .secondary)
                    .lineLimit(2, reservesSpace: false)
                    .fixedSize(horizontal: false, vertical: true)

                bar
            }
        }
    }

    /// The right-hand number: a percentage only while a step is measurably
    /// running. Nothing for the rest, so the column stays quiet.
    private var trailing: String? {
        guard step.state == .running, step.fraction > 0 else { return nil }
        return "\(Int(step.fraction * 100))%"
    }

    /// Every row gets a bar, so the list reads as a column of progress rather
    /// than text that occasionally sprouts one. A running step with no measurable
    /// fraction (the installer wizard, wineboot) gets an indeterminate bar — that
    /// is honest, where a fake creeping fraction is not.
    @ViewBuilder
    private var bar: some View {
        Group {
            if step.state == .running && step.fraction <= 0 {
                ProgressView()
            } else {
                ProgressView(value: barValue)
            }
        }
        .progressViewStyle(.linear)
        .controlSize(.small)
        .tint(barTint)
        .opacity(step.state == .pending ? 0.35 : 1)
    }

    private var barValue: Double {
        switch step.state {
        case .pending: return 0
        case .running: return step.fraction
        case .done, .skipped, .failed: return 1
        }
    }

    private var barTint: Color {
        switch step.state {
        case .failed: return .red
        case .skipped: return .secondary
        case .done: return .green
        default: return .acAccent
        }
    }
}

/// The state marker at the head of a row.
private struct StepIcon: View {
    let state: StepState

    var body: some View {
        switch state {
        case .pending:
            Image(systemName: "circle")
                .foregroundStyle(.tertiary)
        case .running:
            ProgressView()
                .controlSize(.small)
                .scaleEffect(0.6)
        case .done:
            Image(systemName: "checkmark.circle.fill")
                .foregroundStyle(.green)
        case .skipped:
            // Distinct from done on purpose: nothing happened, and that is
            // information — it is why a re-run finishes in two seconds.
            Image(systemName: "minus.circle.fill")
                .foregroundStyle(.secondary)
        case .failed:
            Image(systemName: "exclamationmark.triangle.fill")
                .foregroundStyle(.red)
        }
    }
}
