// acspaces.dylib — macOS "fullscreen Space" support for Asheron's Call under Wine.
//
// This is injected into the Wine process at launch via DYLD_INSERT_LIBRARIES (see
// core/src/wine.rs). Running *inside* the process that owns AC's window is the whole
// point: calling that window's own -toggleFullScreen: needs NO Accessibility
// permission — it is exactly what a native app does when it fullscreens itself. A
// cross-process toggle (from the launcher) would instead require the user to grant
// Accessibility, which we avoid entirely.
//
// It waits until winemac has granted AC's window native-fullscreen capability
// (NSWindowCollectionBehaviorFullScreenPrimary, which winemac sets once the window
// has a resizable frame — the `window-style-create` / `window-style-restyle` patches
// in core/src/patches.rs put one there), then toggles it into its own macOS Space
// and pins it there, so the Space cannot be left by accident.
//
// The window is the display resolution from the very first frame: the companion
// `login-resolution` patch removes the client's hardcoded 800x600 for the splash,
// login and character-select screens, so there is one window of one size for the
// whole session and nothing here has to wait for the world to load.
//
// Those screens still *draw* their artwork at 800x600, in the top-left corner of the
// window, and that is accepted rather than unsolved. It was attempted on 2026-07-26
// by growing the window to the display and letting the renderer scale the picture
// over it, which does not work and cannot be made to: `WINEDEBUG=+d3d` shows every
// present of a session as `src_rect (0,0)-(800,600), dst_rect (0,0)-(800,600)`,
// unchanged whatever the window does, because wined3d resolves the NULL destination
// AC passes to the *backbuffer* size rather than to the window's client rect. A
// bigger window only puts the same picture in the corner of a bigger frame. Scaling
// them for real needs a d3d9 renderer whose present blits through a scaling path
// (DXVK's does; wined3d's does not), or an exclusive-fullscreen 800x600 display
// mode, which a MacBook's built-in panel does not offer.
//
// Rebuild with macos/helpers/build.sh (or core/build.rs does it automatically).

#import <AppKit/AppKit.h>
#import <dispatch/dispatch.h>
#import <objc/runtime.h>

// A floor, not a gate: AC's window is always the display resolution, so this only
// exists to make sure we never fullscreen some incidental Wine dialog instead —
// notably the 327x134 "fatal DirectX issue" box.
static const CGFloat kMinWindowWidth = 640.0;

// Set BETTERAC_SPACES_DEBUG=1 to trace what this is doing onto the client's stderr.
// Worth keeping: everything here is a guess about winemac/AppKit internals until it
// is watched running, and it is not otherwise observable from outside the process.
static void dbg(NSString *fmt, ...) {
    static int on = -1;
    if (on < 0) on = getenv("BETTERAC_SPACES_DEBUG") != NULL;
    if (!on) return;
    va_list ap;
    va_start(ap, fmt);
    NSString *s = [[NSString alloc] initWithFormat:fmt arguments:ap];
    va_end(ap);
    fprintf(stderr, "acspaces: %s\n", s.UTF8String);
}

static IMP gOriginalToggleFullScreen;

// Most ways a player can leave a fullscreen Space — the green button, the Window
// menu's "Exit Full Screen", Ctrl-Cmd-F, an Accessibility client clearing
// AXFullScreen — end up here, at -toggleFullScreen:. Refusing every one of them
// while the window is already fullscreen pins the game in its Space.
//
// Deliberately not conditioned on `sender`: an obvious-looking heuristic is that
// user-driven toggles carry the control that triggered them and programmatic ones
// pass nil, but tracing showed the Accessibility path passes nil too, so that test
// leaks. Nothing here needs the exception anyway — our own calls only ever happen
// when the window is *not* fullscreen, so they are never the ones refused.
//
// Cmd-Tab and Mission Control are unaffected: switching away from a Space does not
// toggle fullscreen, it just moves you to another Space.
static void ac_toggleFullScreen(id self, SEL _cmd, id sender) {
    NSWindow *w = (NSWindow *)self;
    BOOL full = (w.styleMask & NSWindowStyleMaskFullScreen) != 0;
    dbg(@"toggleFullScreen: sender=%@ fullscreen=%d", sender, full);
    if (full) {
        dbg(@"  refused");
        return;
    }
    ((void (*)(id, SEL, id))gOriginalToggleFullScreen)(self, _cmd, sender);
}

// The backstop, for any exit that does not come through -toggleFullScreen: at all —
// dragging the window out of its Space in Mission Control, say. Whenever the window
// reports that it has left fullscreen, put it straight back.
//
// The delay lets a legitimate transient settle before we act: AC changing display
// mode, or the window being torn down at quit, would both surface as an exit first.
static void reenterAfterExit(NSNotification *note) {
    NSWindow *w = note.object;
    // A spin guard, not a policy: if something in the driver genuinely cannot keep
    // this window fullscreen, bouncing it forever would make the game unusable, so
    // give up rather than fight. A player cannot realistically reach this by hand.
    static int reentries = 0;
    if (reentries >= 20) {
        dbg(@"exited fullscreen, but the re-entry limit is reached -- leaving it alone");
        return;
    }
    reentries++;
    dbg(@"exited fullscreen, will re-enter (%d)", reentries);
    dispatch_after(dispatch_time(DISPATCH_TIME_NOW, (int64_t)(0.5 * NSEC_PER_SEC)),
                   dispatch_get_main_queue(), ^{
        @try {
            if (!w.isVisible) return;
            if (w.styleMask & NSWindowStyleMaskFullScreen) return;
            if (!(w.collectionBehavior & NSWindowCollectionBehaviorFullScreenPrimary)) return;
            dbg(@"re-entering fullscreen");
            [w toggleFullScreen:nil];
        } @catch (NSException *e) {
        }
    });
}

// Swizzle once. `class_getInstanceMethod` walks up to whichever class actually
// implements it (NSWindow, in practice), so this covers every window in the Wine
// process — which is what we want anyway: AC's is the only one that is ever
// fullscreen, and a stray Wine dialog has nothing to exit.
static void pinFullScreen(NSWindow *w) {
    if (gOriginalToggleFullScreen) return;
    Method m = class_getInstanceMethod([w class], @selector(toggleFullScreen:));
    if (!m) {
        dbg(@"no toggleFullScreen: on %@ -- cannot pin", [w class]);
        return;
    }
    gOriginalToggleFullScreen = method_setImplementation(m, (IMP)ac_toggleFullScreen);
    dbg(@"pinned toggleFullScreen: on %@", [w class]);

    [[NSNotificationCenter defaultCenter]
        addObserverForName:NSWindowDidExitFullScreenNotification
                    object:w
                     queue:nil
                usingBlock:^(NSNotification *note) { reenterAfterExit(note); }];
}

static NSWindow *candidateWindow(void) {
    NSApplication *app = NSApp;
    if (!app) return nil;                     // a Wine child process with no GUI
    for (NSWindow *w in [app windows]) {
        if (![w respondsToSelector:@selector(toggleFullScreen:)]) continue;
        if (w.styleMask & NSWindowStyleMaskFullScreen) continue;           // already a Space
        if (!(w.collectionBehavior & NSWindowCollectionBehaviorFullScreenPrimary)) continue;
        if (!w.isVisible) continue;
        if (w.frame.size.width < kMinWindowWidth) continue;
        return w;
    }
    return nil;
}

static void startPolling(void) {
    static dispatch_source_t timer;
    dispatch_queue_t q = dispatch_get_main_queue();
    timer = dispatch_source_create(DISPATCH_SOURCE_TYPE_TIMER, 0, 0, q);
    dispatch_source_set_timer(timer, dispatch_time(DISPATCH_TIME_NOW, 0),
                              (uint64_t)(0.4 * NSEC_PER_SEC), (uint64_t)(0.1 * NSEC_PER_SEC));
    __block int ticks = 0;
    __block NSWindow *pending = nil;
    dispatch_source_set_event_handler(timer, ^{
        NSWindow *w = nil;
        @try {
            w = candidateWindow();
        } @catch (NSException *e) {
            // A window vanishing mid-iteration must never take AC down with it.
        }
        // Require the same window to look ready twice running before acting. AppKit
        // reports a window as capable and full-size part-way through its own
        // fullscreen animation, and toggling then would fight the transition (and
        // burn our one shot); a settled window reads the same on two ticks.
        if (w && w == pending) {
            @try {
                pinFullScreen(w);          // before, so the entry itself is the last toggle
                [w toggleFullScreen:nil];  // nil sender: our own call is never refused
            } @catch (NSException *e) {}
            dispatch_source_cancel(timer);
            return;
        }
        pending = w;
        // Give up after ~10 min if no window ever becomes fullscreen-capable.
        if (++ticks > 1500) dispatch_source_cancel(timer);
    });
    dispatch_resume(timer);
}

__attribute__((constructor))
static void acspaces_init(void) {
    // The AppKit run loop is not up yet at load; defer onto the main queue so the
    // polling timer schedules once winemac's NSApplication is running.
    dispatch_async(dispatch_get_main_queue(), ^{ startPolling(); });
}
