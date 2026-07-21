//! Building the acclient.exe command line.
//!
//! This is the part with a real trap in it: ACE and GDLE take the same three
//! facts -- account, password, address -- in two completely different shapes. Get
//! it wrong and the client just fails to log in.
//!
//!   ACE    acclient.exe -a ACCOUNT -v PASSWORD -h HOST:PORT
//!   GDLE   acclient.exe -h HOST -p PORT -a ACCOUNT:PASSWORD
//!
//! Note GDLE colon-joins the credentials into one argument. A password containing
//! a ':' is therefore ambiguous on GDLE and we reject it up front rather than let
//! the user stare at a login failure they cannot explain.
//!
//! Everything here is pure -- no processes, no environment, no toolkit. That is
//! why it carries the test suite: the command shape is a value we can assert on
//! rather than something we hope is right at spawn time.

use crate::servers::{Server, Software};

/// Reject credentials the client cannot express, before we launch and fail
/// obscurely. Empty is caught here too -- acclient just sits at a dead login
/// screen if you hand it a blank account.
pub fn validate(server: &Server, account: &str, password: &str) -> Result<(), String> {
    if account.trim().is_empty() {
        return Err("Enter an account name.".into());
    }
    if password.is_empty() {
        return Err("Enter a password.".into());
    }
    if account.contains(':') {
        return Err("Account names cannot contain a colon.".into());
    }
    // Only GDLE is ambiguous here: it packs account:password into one argument,
    // so a ':' in the password would silently split in the wrong place. ACE
    // passes the password as its own -v argument and does not care.
    if server.software == Software::Gdle && password.contains(':') {
        return Err("This server runs GDLE, which cannot accept a password containing a colon.".into());
    }
    Ok(())
}

/// The acclient.exe arguments for this server and account.
///
/// `-rodat off` makes the client tolerate the End-of-Retail dats; without it it
/// refuses to start against a patched data set.
pub fn client_args(server: &Server, account: &str, password: &str) -> Vec<String> {
    let mut a: Vec<String> = match server.software {
        Software::Ace => vec![
            "-a".into(), account.into(),
            "-v".into(), password.into(),
            "-h".into(), server.address(),
        ],
        Software::Gdle => vec![
            "-h".into(), server.host.clone(),
            "-p".into(), server.port.clone(),
            "-a".into(), format!("{account}:{password}"),
        ],
    };
    a.push("-rodat".into());
    a.push("off".into());
    a
}

/// The gamescope flags we always pass.
///
/// `-f` fullscreens the nested compositor. `--force-grab-cursor` keeps the mouse
/// inside it: AC is a click-to-move game and without this the cursor walks off
/// the game onto the desktop mid-fight.
pub const GAMESCOPE_FLAGS: &[&str] = &["-f", "--force-grab-cursor"];

/// The gamescope argv for a display of this size.
///
/// The resolution flags are not optional decoration. Nested gamescope defaults to
/// **1280x720**, not to the size of your screen, so `-f` on its own fullscreens an
/// upscaled 720p image and the game looks soft for no reason. `-W/-H` is the
/// output (the window gamescope opens); `-w/-h` is the resolution the game is told
/// it has. Setting all four to the current mode means native, 1:1, no scaling.
///
/// If we cannot work out the resolution we pass neither, because a wrong `-W/-H`
/// is worse than gamescope's own guess.
pub fn gamescope_args_for(res: Option<(i32, i32)>) -> Vec<String> {
    let mut args: Vec<String> = Vec::new();
    if let Some((w, h)) = res {
        for (flag, v) in [("-W", w), ("-H", h), ("-w", w), ("-h", h)] {
            args.push(flag.into());
            args.push(v.to_string());
        }
    }
    args.extend(GAMESCOPE_FLAGS.iter().map(|s| s.to_string()));
    args
}

/// What we are actually going to exec: the program, its argv, and whether Proton
/// should fall back to wined3d. Split out from launching so the shape of the
/// command is a thing tests can look at rather than something we hope is right.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Invocation {
    pub program: String,
    pub args: Vec<String>,
    pub wined3d: bool,
}

/// Build the command line.
///
/// Under gamescope we deliberately turn DXVK off (PROTON_USE_WINED3D=1). The two
/// go together: wined3d could not enumerate a display adapter on a bare
/// Wayland/XWayland session -- that is the "Cannot initialize Direct3D" that made
/// this project reach for DXVK in the first place -- but gamescope is a nested
/// compositor that hands the client a display of its own, which is the thing that
/// was missing. AC is a D3D9 game from 1999; it does not need a Vulkan translation
/// layer once it can see a screen.
///
/// Without gamescope we leave Proton alone and DXVK stays on, because there the
/// old failure is still real.
pub fn invocation(
    server: &Server,
    account: &str,
    password: &str,
    gamescope: bool,
    gamescope_args: &[String],
) -> Invocation {
    let client = std::iter::once("acclient.exe".to_string())
        .chain(client_args(server, account, password));

    if gamescope {
        let mut args: Vec<String> = gamescope_args.to_vec();
        args.push("--".into());
        args.push("umu-run".into());
        args.extend(client);
        Invocation { program: "gamescope".into(), args, wined3d: true }
    } else {
        Invocation { program: "umu-run".into(), args: client.collect(), wined3d: false }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::servers::Players;

    fn srv(software: Software) -> Server {
        Server {
            name: "Coldeve".into(),
            description: String::new(),
            ruleset: "PvE".into(),
            software,
            host: "play.coldeve.ac".into(),
            port: "9000".into(),
            players: Some(Players { count: 765, age: "7 minutes ago".into() }),
            website_url: None,
            discord_url: None,
        }
    }

    #[test]
    fn ace_passes_the_password_as_its_own_argument() {
        let a = client_args(&srv(Software::Ace), "hank", "hunter2");
        assert_eq!(
            a,
            ["-a", "hank", "-v", "hunter2", "-h", "play.coldeve.ac:9000", "-rodat", "off"]
        );
    }

    #[test]
    fn gdle_colon_joins_the_credentials_and_splits_the_address() {
        let a = client_args(&srv(Software::Gdle), "hank", "hunter2");
        assert_eq!(
            a,
            ["-h", "play.coldeve.ac", "-p", "9000", "-a", "hank:hunter2", "-rodat", "off"]
        );
    }

    #[test]
    fn the_two_shapes_are_genuinely_different() {
        // Guards the actual bug this module exists to prevent: handing GDLE the
        // ACE argv (or vice versa) logs you in nowhere.
        let ace = client_args(&srv(Software::Ace), "hank", "pw");
        let gdle = client_args(&srv(Software::Gdle), "hank", "pw");
        assert_ne!(ace, gdle);
        assert!(ace.contains(&"play.coldeve.ac:9000".to_string()));
        assert!(gdle.contains(&"hank:pw".to_string()));
    }

    #[test]
    fn a_colon_password_is_refused_on_gdle_but_fine_on_ace() {
        let bad = "hun:ter2";
        assert!(validate(&srv(Software::Gdle), "hank", bad).is_err());
        assert!(validate(&srv(Software::Ace), "hank", bad).is_ok());
        // and it really would have split wrong
        let a = client_args(&srv(Software::Gdle), "hank", bad);
        assert!(a.contains(&"hank:hun:ter2".to_string()));
    }

    #[test]
    fn empty_credentials_are_refused() {
        assert!(validate(&srv(Software::Ace), "", "pw").is_err());
        assert!(validate(&srv(Software::Ace), "hank", "").is_err());
        assert!(validate(&srv(Software::Ace), "  ", "pw").is_err());
    }

    #[test]
    fn rodat_off_is_always_present() {
        for sw in [Software::Ace, Software::Gdle] {
            let a = client_args(&srv(sw), "u", "p");
            let i = a.iter().position(|x| x == "-rodat").expect("-rodat missing");
            assert_eq!(a[i + 1], "off");
        }
    }

    fn gs_args() -> Vec<String> {
        gamescope_args_for(Some((5504, 2304)))
    }

    #[test]
    fn the_display_resolution_is_passed_as_both_output_and_game_resolution() {
        // Nested gamescope defaults to 1280x720, so omitting these is not "native",
        // it is 720p upscaled. -W/-H is the window, -w/-h is what the game is told.
        assert_eq!(
            gamescope_args_for(Some((5504, 2304))),
            [
                "-W", "5504", "-H", "2304",
                "-w", "5504", "-h", "2304",
                "-f", "--force-grab-cursor",
            ]
        );
    }

    #[test]
    fn an_undetectable_resolution_passes_no_resolution_at_all() {
        // A wrong -W/-H is worse than gamescope's own guess.
        let a = gamescope_args_for(None);
        assert_eq!(a, ["-f", "--force-grab-cursor"]);
        assert!(!a.iter().any(|x| x == "-W" || x == "-w"));
    }

    #[test]
    fn gamescope_wraps_umu_run_and_the_client_argv_survives_the_separator() {
        let inv = invocation(&srv(Software::Ace), "hank", "hunter2", true, &gs_args());
        assert_eq!(inv.program, "gamescope");
        assert_eq!(
            inv.args,
            [
                "-W", "5504", "-H", "2304", "-w", "5504", "-h", "2304",
                "-f", "--force-grab-cursor", "--", "umu-run", "acclient.exe",
                "-a", "hank", "-v", "hunter2", "-h", "play.coldeve.ac:9000",
                "-rodat", "off",
            ]
        );
        // The client's own flags must land after the separator, or gamescope eats
        // them as its own and the game launches with no credentials at all.
        let sep = inv.args.iter().position(|a| a == "--").unwrap();
        let umu = inv.args.iter().position(|a| a == "umu-run").unwrap();
        assert!(sep < umu);
        assert!(inv.args.iter().position(|a| a == "-rodat").unwrap() > umu);
    }

    #[test]
    fn gamescope_h_and_the_clients_h_do_not_collide() {
        // Both use -h: gamescope means "game height", acclient means "host". They
        // are only unambiguous because one is before the -- and one is after it.
        let inv = invocation(&srv(Software::Ace), "hank", "pw", true, &gs_args());
        let sep = inv.args.iter().position(|a| a == "--").unwrap();
        let hs: Vec<usize> =
            inv.args.iter().enumerate().filter(|(_, a)| *a == "-h").map(|(i, _)| i).collect();
        assert_eq!(hs.len(), 2, "expected gamescope's -h and the client's -h");
        assert!(hs[0] < sep, "gamescope's -h must precede the separator");
        assert!(hs[1] > sep, "the client's -h must follow it");
        assert_eq!(inv.args[hs[0] + 1], "2304");
        assert_eq!(inv.args[hs[1] + 1], "play.coldeve.ac:9000");
    }

    #[test]
    fn turning_gamescope_on_turns_dxvk_off_and_leaving_it_off_leaves_dxvk_on() {
        // The coupling is the whole point: wined3d cannot enumerate a display on a
        // bare Wayland session, so it is only safe once gamescope provides one.
        assert!(invocation(&srv(Software::Ace), "u", "p", true, &gs_args()).wined3d);
        assert!(!invocation(&srv(Software::Ace), "u", "p", false, &gs_args()).wined3d);
    }

    #[test]
    fn without_gamescope_we_exec_umu_run_directly() {
        let inv = invocation(&srv(Software::Gdle), "hank", "hunter2", false, &gs_args());
        assert_eq!(inv.program, "umu-run");
        assert_eq!(inv.args[0], "acclient.exe");
        assert!(!inv.args.contains(&"--".to_string()));
        assert!(inv.args.contains(&"hank:hunter2".to_string()));
    }

    #[test]
    fn custom_gamescope_args_replace_the_defaults_but_not_the_client() {
        let custom: Vec<String> =
            "-w 2752 -h 1152 -F fsr -f".split_whitespace().map(String::from).collect();
        let inv = invocation(&srv(Software::Ace), "hank", "pw", true, &custom);
        assert_eq!(&inv.args[..6], ["-w", "2752", "-h", "1152", "-F", "fsr"]);
        assert!(!inv.args.contains(&"--force-grab-cursor".to_string()));
        assert!(inv.args.contains(&"acclient.exe".to_string()));
        assert!(inv.args.contains(&"play.coldeve.ac:9000".to_string()));
    }
}
