//! `hamzip upgrade` and the once-a-day update notice on stderr.

use std::io::IsTerminal;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use hamzip::{net, Error};

fn confirm(question: &str) -> bool {
    eprint!("{question} [y/N] ");
    let mut line = String::new();
    std::io::stdin().read_line(&mut line).is_ok() && line.trim().eq_ignore_ascii_case("y")
}

/// `hamzip upgrade`: replace this hamzip, and hamzip-gui beside it if there is
/// one, with the latest release's builds.
pub fn upgrade(check_only: bool, yes: bool) -> Result<(), Error> {
    let rel = net::latest_release(Duration::from_secs(30))?;
    let cur = net::current();
    let Some(new) = net::newer(&rel) else {
        println!("hamzip {cur} is up to date");
        return Ok(());
    };
    println!(
        "hamzip {new} is available (you have {cur}): {}",
        rel.html_url
    );
    if check_only {
        return Ok(());
    }

    let exe =
        std::env::current_exe().map_err(|e| Error::Io(format!("finding this executable: {e}")))?;
    let mut programs = exe
        .parent()
        .map(net::installed_programs)
        .unwrap_or_default();
    // a portable copy may have been renamed, so the running exe is always included
    if !programs.iter().any(|(p, _)| *p == exe) {
        programs.retain(|(_, name)| *name != "hamzip");
        programs.insert(0, (exe.clone(), "hamzip"));
    }
    let list: Vec<String> = programs
        .iter()
        .map(|(p, _)| p.display().to_string())
        .collect();
    if !yes && !confirm(&format!("Replace {} with {new}?", list.join(" and "))) {
        println!("not installed");
        return Ok(());
    }
    net::install(&rel, &programs)?;
    println!("installed hamzip {new}");
    Ok(())
}

/// Starts the once-a-day update check, unless it is turned off, already ran
/// today, or nobody would see the notice. Returns a handle to read at exit.
pub fn start_notice(data_dir: &Path) -> Option<std::sync::mpsc::Receiver<String>> {
    let off =
        std::env::var_os("HAMZIP_NO_UPDATE_CHECK").is_some() || std::env::var_os("CI").is_some();
    if off || !std::io::stderr().is_terminal() {
        return None;
    }
    let stamp = data_dir.join("update-check");
    let now = SystemTime::now().duration_since(UNIX_EPOCH).ok()?.as_secs();
    let last: u64 = std::fs::read_to_string(&stamp)
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0);
    // a stamp from the future means the clock was wrong once; don't wait for it forever
    if last <= now && now - last < 24 * 3600 {
        return None;
    }
    // record the attempt, not the success, so an offline station isn't slowed every run
    std::fs::create_dir_all(data_dir).ok()?;
    std::fs::write(&stamp, now.to_string()).ok()?;
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        if let Some(v) = net::latest_release(Duration::from_secs(5))
            .ok()
            .and_then(|r| net::newer(&r))
        {
            let _ = tx.send(format!(
                "hamzip {v} is available (you have {}); run `hamzip upgrade`",
                net::current()
            ));
        }
    });
    Some(rx)
}
