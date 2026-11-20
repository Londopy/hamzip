//! Network features: `hamzip upgrade`, the once-a-day update notice, and
//! downloading the shared model. Every download is checked against a SHA-256
//! before it is used.

use std::fs::File;
use std::io::{IsTerminal, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use semver::Version;
use serde::Deserialize;
use sha2::{Digest, Sha256};

use hamzip::Error;

const REPO: &str = "Londopy/hamzip";
pub const MODEL_FILE: &str = "smollm2-135m.hzm";
const MODEL_SHA256: &str = "4f3b006a37c45d5f7ac496eba7bae7deef47661feeed1891b3340e18b4b9c919";
// the model lives on its own release so app releases don't re-upload 137 MB each time
const MODEL_URL: &str =
    "https://github.com/Londopy/hamzip/releases/download/model-smollm2-135m/smollm2-135m.hzm";

#[derive(Deserialize)]
struct Release {
    tag_name: String,
    html_url: String,
    assets: Vec<Asset>,
}

#[derive(Deserialize)]
struct Asset {
    name: String,
    browser_download_url: String,
}

// reqwest's own message is often just "builder error"; the cause is further down the chain
fn net_err(e: impl std::error::Error) -> Error {
    let mut msg = format!("network: {e}");
    let mut src = e.source();
    while let Some(s) = src {
        msg.push_str(&format!(": {s}"));
        src = s.source();
    }
    Error::Io(msg)
}

fn client(timeout: Duration) -> Result<reqwest::blocking::Client, Error> {
    reqwest::blocking::Client::builder()
        .user_agent(concat!("hamzip/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(15).min(timeout))
        .timeout(timeout)
        .build()
        .map_err(net_err)
}

fn latest(timeout: Duration) -> Result<Release, Error> {
    client(timeout)?
        .get(format!(
            "https://api.github.com/repos/{REPO}/releases/latest"
        ))
        .header("Accept", "application/vnd.github+json")
        .send()
        .and_then(|r| r.error_for_status())
        .and_then(|r| r.json())
        .map_err(net_err)
}

fn version_of(tag: &str) -> Option<Version> {
    Version::parse(tag.trim_start_matches('v')).ok()
}

fn current() -> Version {
    Version::parse(env!("CARGO_PKG_VERSION")).expect("crate version is semver")
}

/// Where this platform's standalone binary sits in a release.
fn asset_name(ver: &Version) -> Option<String> {
    let arch = match (std::env::consts::OS, std::env::consts::ARCH) {
        ("windows", "x86_64") => "x64-portable.exe",
        ("windows", "x86") => "x86-portable.exe",
        ("windows", "aarch64") => "arm64-portable.exe",
        ("linux", "x86_64") => "linux-x86_64",
        ("linux", "aarch64") => "linux-aarch64",
        ("linux", "arm") => "linux-armv7",
        _ => return None,
    };
    Some(format!("hamzip-v{ver}-{arch}"))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn confirm(question: &str) -> bool {
    eprint!("{question} [y/N] ");
    let mut line = String::new();
    std::io::stdin().read_line(&mut line).is_ok() && line.trim().eq_ignore_ascii_case("y")
}

/// `hamzip upgrade`: replace this executable with the latest release's.
pub fn upgrade(check_only: bool, yes: bool) -> Result<(), Error> {
    let rel = latest(Duration::from_secs(30))?;
    let new = version_of(&rel.tag_name).ok_or_else(|| {
        Error::Io(format!(
            "latest release has a non-semver tag {}",
            rel.tag_name
        ))
    })?;
    let cur = current();
    if new <= cur {
        println!("hamzip {cur} is up to date");
        return Ok(());
    }
    println!(
        "hamzip {new} is available (you have {cur}): {}",
        rel.html_url
    );
    if check_only {
        return Ok(());
    }

    let name = asset_name(&new).ok_or_else(|| {
        Error::Io(format!(
            "no release build for this platform; see {}",
            rel.html_url
        ))
    })?;
    let find = |n: &str| {
        rel.assets
            .iter()
            .find(|a| a.name == n)
            .map(|a| a.browser_download_url.clone())
            .ok_or_else(|| Error::Io(format!("release {} has no {n}", rel.tag_name)))
    };
    let (exe_url, sums_url) = (find(&name)?, find("SHA256SUMS.txt")?);

    let c = client(Duration::from_secs(600))?;
    let sums = c
        .get(sums_url)
        .send()
        .and_then(|r| r.error_for_status())
        .and_then(|r| r.text());
    let sums = sums.map_err(net_err)?;
    let want = sums_entry(&sums, &name).ok_or_else(|| {
        Error::Io(format!(
            "SHA256SUMS.txt has no entry for {name}; not installing"
        ))
    })?;
    let bytes = c
        .get(exe_url)
        .send()
        .and_then(|r| r.error_for_status())
        .and_then(|r| r.bytes());
    let bytes = bytes.map_err(net_err)?;
    if hex(&Sha256::digest(&bytes)) != want {
        return Err(Error::Io(format!(
            "{name} does not match its SHA-256; not installing"
        )));
    }

    let exe =
        std::env::current_exe().map_err(|e| Error::Io(format!("finding this executable: {e}")))?;
    if !yes && !confirm(&format!("Replace {} with hamzip {new}?", exe.display())) {
        println!("not installed");
        return Ok(());
    }
    replace_exe(&exe, &bytes, &["--version"]).map_err(|e| {
        Error::Io(format!(
            "could not replace {}: {e}. Nothing was changed; install {new} from {} instead",
            exe.display(),
            rel.html_url
        ))
    })?;
    println!("installed hamzip {new}");
    Ok(())
}

fn old_path(exe: &Path) -> PathBuf {
    exe.with_extension("old")
}

/// Writes the new program beside the old one and starts it once with `probe`
/// (a build for a newer glibc fails here, not after the swap). Only then is the
/// running exe, which Windows can rename but not overwrite, moved aside.
fn replace_exe(exe: &Path, bytes: &[u8], probe: &[&str]) -> std::io::Result<()> {
    let staged = exe.with_extension("new");
    let fail = |e: std::io::Error| {
        let _ = std::fs::remove_file(&staged);
        e
    };
    File::create(&staged)
        .and_then(|mut f| f.write_all(bytes).and_then(|_| f.sync_all()))
        .map_err(fail)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&staged, std::fs::Permissions::from_mode(0o755)).map_err(fail)?;
    }
    let runs = std::process::Command::new(&staged)
        .args(probe)
        .output()
        .is_ok_and(|o| o.status.success());
    if !runs {
        return Err(fail(std::io::Error::other(
            "the downloaded program does not start on this machine",
        )));
    }
    let old = old_path(exe);
    let _ = std::fs::remove_file(&old);
    std::fs::rename(exe, &old).map_err(fail)?;
    if let Err(e) = std::fs::rename(&staged, exe) {
        let _ = std::fs::rename(&old, exe);
        return Err(fail(e));
    }
    Ok(())
}

/// Deletes the copy `upgrade` left behind, once the new exe is the one running.
pub fn clean_old_exe() {
    if let Ok(exe) = std::env::current_exe() {
        let _ = std::fs::remove_file(old_path(&exe));
    }
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
        if let Ok(rel) = latest(Duration::from_secs(5)) {
            if version_of(&rel.tag_name).is_some_and(|v| v > current()) {
                let _ = tx.send(format!(
                    "hamzip {} is available (you have {}); run `hamzip upgrade`",
                    rel.tag_name.trim_start_matches('v'),
                    current()
                ));
            }
        }
    });
    Some(rx)
}

/// `hamzip model download`: fetch the shared model and check its SHA-256.
pub fn download_model(dir: &Path) -> Result<PathBuf, Error> {
    download_checked(MODEL_URL, MODEL_SHA256, &dir.join(MODEL_FILE))
}

/// Streams `url` to `dest`, which only appears once its SHA-256 matches.
fn download_checked(url: &str, sha256: &str, dest: &Path) -> Result<PathBuf, Error> {
    let dir = dest.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(dir).map_err(|e| Error::Io(format!("{}: {e}", dir.display())))?;
    let part = dest.with_extension("part");
    let fetched = stream_to(url, &part);
    if fetched.as_deref().ok() != Some(sha256) {
        let _ = std::fs::remove_file(&part);
        fetched?;
        return Err(Error::Io(format!(
            "{url} does not match its SHA-256; deleted the download"
        )));
    }
    std::fs::rename(&part, dest).map_err(|e| Error::Io(format!("{}: {e}", dest.display())))?;
    Ok(dest.to_path_buf())
}

/// Writes `url` to `path` and returns the SHA-256 of what arrived.
fn stream_to(url: &str, path: &Path) -> Result<String, Error> {
    // reqwest applies this per read once the headers are in, so a slow but steady
    // download is fine and a dead link gives up within a minute
    let mut resp = client(Duration::from_secs(60))?
        .get(url)
        .send()
        .and_then(|r| r.error_for_status())
        .map_err(net_err)?;
    let total = resp.content_length();
    let file_err = |e: std::io::Error| Error::Io(format!("{}: {e}", path.display()));
    let mut file = File::create(path).map_err(file_err)?;
    let mut hash = Sha256::new();
    let (mut buf, mut got, mut shown) = (vec![0u8; 1 << 16], 0u64, 0u64);
    loop {
        let n = resp.read(&mut buf).map_err(net_err)?;
        if n == 0 {
            break;
        }
        hash.update(&buf[..n]);
        file.write_all(&buf[..n]).map_err(file_err)?;
        got += n as u64;
        if got - shown >= 16 << 20 {
            shown = got;
            match total {
                Some(t) => eprintln!("{} of {} MB", got >> 20, t >> 20),
                None => eprintln!("{} MB", got >> 20),
            }
        }
    }
    Ok(hex(&hash.finalize()))
}

/// The hash `SHA256SUMS.txt` lists for `name`, in `sha256sum` format.
fn sums_entry(sums: &str, name: &str) -> Option<String> {
    sums.lines()
        .filter_map(|l| l.split_once(char::is_whitespace))
        .find(|(_, file)| file.trim().trim_start_matches('*') == name)
        .map(|(hash, _)| hash.to_ascii_lowercase())
}

#[cfg(test)]
mod tests {
    use std::io::{BufRead, BufReader};
    use std::net::TcpListener;

    use super::*;

    // serves `body` once over plain HTTP, standing in for the GitHub download
    fn serve_once(body: &'static [u8]) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/file", listener.local_addr().unwrap());
        std::thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            let mut r = BufReader::new(s.try_clone().unwrap());
            let mut line = String::new();
            while r.read_line(&mut line).unwrap() > 2 {
                line.clear();
            }
            let head = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            s.write_all(head.as_bytes()).unwrap();
            s.write_all(body).unwrap();
        });
        url
    }

    fn temp_dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("hamzip-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn download_keeps_a_file_whose_hash_matches() {
        let body: &[u8] = b"model bytes";
        let dest = temp_dir("ok").join("m.hzm");
        let got = download_checked(&serve_once(body), &hex(&Sha256::digest(body)), &dest).unwrap();
        assert_eq!(std::fs::read(got).unwrap(), body);
    }

    #[test]
    fn download_with_wrong_hash_leaves_nothing() {
        let dir = temp_dir("bad");
        let dest = dir.join("m.hzm");
        let err = download_checked(&serve_once(b"tampered"), &"0".repeat(64), &dest).unwrap_err();
        assert!(err.to_string().contains("SHA-256"), "{err}");
        assert!(!dest.exists() && !dest.with_extension("part").exists());
    }

    #[test]
    fn sums_lookup_reads_sha256sum_format() {
        let sums = "AB12  hamzip-v0.2.0-x64-portable.exe\ncd34 *hamzip-v0.2.0-linux-aarch64\n";
        assert_eq!(
            sums_entry(sums, "hamzip-v0.2.0-x64-portable.exe").as_deref(),
            Some("ab12")
        );
        assert_eq!(
            sums_entry(sums, "hamzip-v0.2.0-linux-aarch64").as_deref(),
            Some("cd34")
        );
        assert_eq!(sums_entry(sums, "hamzip-v0.2.0-x86-portable.exe"), None);
    }

    #[test]
    fn replace_exe_swaps_in_a_program_that_runs() {
        let dir = temp_dir("swap");
        std::fs::create_dir_all(&dir).unwrap();
        let exe = dir.join("hamzip.exe");
        std::fs::write(&exe, b"old").unwrap();
        // this test binary is a real program; `--list` makes it exit 0
        let new = std::fs::read(std::env::current_exe().unwrap()).unwrap();
        replace_exe(&exe, &new, &["--list"]).unwrap();
        assert_eq!(std::fs::read(&exe).unwrap(), new);
        assert_eq!(std::fs::read(old_path(&exe)).unwrap(), b"old");
        assert!(!exe.with_extension("new").exists());
    }

    #[test]
    fn replace_exe_keeps_the_old_one_when_the_new_does_not_start() {
        let dir = temp_dir("junk");
        std::fs::create_dir_all(&dir).unwrap();
        let exe = dir.join("hamzip.exe");
        std::fs::write(&exe, b"old").unwrap();
        let err = replace_exe(&exe, b"not a program", &["--version"]).unwrap_err();
        assert!(err.to_string().contains("does not start"), "{err}");
        assert_eq!(std::fs::read(&exe).unwrap(), b"old");
        assert!(!exe.with_extension("new").exists() && !old_path(&exe).exists());
    }
}
