//! The network side, shared by the CLI and the GUI: published releases, model
//! downloads and replacing the installed programs. Every download is checked
//! against a SHA-256 before it is used.

use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use semver::Version;
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::Error;

pub const REPO: &str = "Londopy/hamzip";

/// The programs a hamzip install can hold; `upgrade` replaces each one present.
pub const PROGRAMS: [&str; 2] = ["hamzip", "hamzip-gui"];

#[derive(Deserialize)]
pub struct Release {
    pub tag_name: String,
    pub html_url: String,
    pub assets: Vec<Asset>,
}

#[derive(Deserialize)]
pub struct Asset {
    pub name: String,
    pub browser_download_url: String,
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

pub fn latest_release(timeout: Duration) -> Result<Release, Error> {
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

pub fn version_of(tag: &str) -> Option<Version> {
    Version::parse(tag.trim_start_matches('v')).ok()
}

/// The version of this hamzip.
pub fn current() -> Version {
    Version::parse(crate::VERSION).expect("crate version is semver")
}

/// The release's version, if it is newer than this hamzip.
pub fn newer(rel: &Release) -> Option<Version> {
    version_of(&rel.tag_name).filter(|v| *v > current())
}

/// Where this platform's standalone build of `program` sits in a release.
pub fn asset_name(program: &str, ver: &Version) -> Option<String> {
    let arch = match (std::env::consts::OS, std::env::consts::ARCH) {
        ("windows", "x86_64") => "x64-portable.exe",
        ("windows", "x86") => "x86-portable.exe",
        ("windows", "aarch64") => "arm64-portable.exe",
        ("linux", "x86_64") => "linux-x86_64",
        ("linux", "aarch64") => "linux-aarch64",
        ("linux", "arm") => "linux-armv7",
        _ => return None,
    };
    Some(format!("{program}-v{ver}-{arch}"))
}

/// The hamzip programs installed in `dir`, with the name their releases use.
pub fn installed_programs(dir: &Path) -> Vec<(PathBuf, &'static str)> {
    PROGRAMS
        .iter()
        .map(|p| (dir.join(format!("{p}{}", std::env::consts::EXE_SUFFIX)), *p))
        .filter(|(path, _)| path.is_file())
        .collect()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Downloads every program's build from `rel` and checks each against
/// `SHA256SUMS.txt` before anything is replaced, then swaps them in one by one.
/// Returns the paths that were replaced.
pub fn install(rel: &Release, programs: &[(PathBuf, &str)]) -> Result<Vec<PathBuf>, Error> {
    let ver = version_of(&rel.tag_name).ok_or_else(|| {
        Error::Io(format!(
            "latest release has a non-semver tag {}",
            rel.tag_name
        ))
    })?;
    let find = |n: &str| {
        rel.assets
            .iter()
            .find(|a| a.name == n)
            .map(|a| a.browser_download_url.clone())
            .ok_or_else(|| Error::Io(format!("release {} has no {n}", rel.tag_name)))
    };
    let c = client(Duration::from_secs(600))?;
    let get = |url: String| {
        c.get(url)
            .send()
            .and_then(|r| r.error_for_status())
            .and_then(|r| r.bytes())
            .map_err(net_err)
    };
    let sums = get(find("SHA256SUMS.txt")?)?;
    let sums = String::from_utf8_lossy(&sums);

    let mut fetched = Vec::new();
    for (path, program) in programs {
        let name = asset_name(program, &ver).ok_or_else(|| {
            Error::Io(format!(
                "no release build for this platform; see {}",
                rel.html_url
            ))
        })?;
        let want = sums_entry(&sums, &name).ok_or_else(|| {
            Error::Io(format!(
                "SHA256SUMS.txt has no entry for {name}; not installing"
            ))
        })?;
        let bytes = get(find(&name)?)?;
        if hex(&Sha256::digest(&bytes)) != want {
            return Err(Error::Io(format!(
                "{name} does not match its SHA-256; not installing"
            )));
        }
        fetched.push((path, bytes));
    }
    let mut done = Vec::new();
    for (path, bytes) in fetched {
        replace_exe(path, &bytes, &["--version"]).map_err(|e| {
            Error::Io(format!(
                "could not replace {}: {e}. Install {ver} from {} instead",
                path.display(),
                rel.html_url
            ))
        })?;
        done.push(path.clone());
    }
    Ok(done)
}

pub fn old_path(exe: &Path) -> PathBuf {
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

/// Deletes the copy an upgrade left behind, once the new program is running.
pub fn clean_old_exe() {
    if let Ok(exe) = std::env::current_exe() {
        let _ = std::fs::remove_file(old_path(&exe));
    }
}

/// Downloads a published model into `dir` and checks its SHA-256. `progress`
/// gets the bytes so far and the total, when the server says it. Each model
/// lives on its own release, so app releases don't re-upload it.
pub fn download_model(
    name: &str,
    dir: &Path,
    progress: &mut dyn FnMut(u64, Option<u64>),
) -> Result<PathBuf, Error> {
    let sha = crate::model_sha256(name).ok_or_else(|| {
        let names: Vec<_> = crate::MODELS.iter().map(|(n, _)| *n).collect();
        Error::Io(format!(
            "no published model named {name}; there are {}",
            names.join(", ")
        ))
    })?;
    let url = format!("https://github.com/{REPO}/releases/download/model-{name}/{name}.hzm");
    download_checked(&url, sha, &dir.join(format!("{name}.hzm")), progress)
}

/// Streams `url` to `dest`, which only appears once its SHA-256 matches.
fn download_checked(
    url: &str,
    sha256: &str,
    dest: &Path,
    progress: &mut dyn FnMut(u64, Option<u64>),
) -> Result<PathBuf, Error> {
    let dir = dest.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(dir).map_err(|e| Error::Io(format!("{}: {e}", dir.display())))?;
    let part = dest.with_extension("part");
    let fetched = stream_to(url, &part, progress);
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
fn stream_to(
    url: &str,
    path: &Path,
    progress: &mut dyn FnMut(u64, Option<u64>),
) -> Result<String, Error> {
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
    let (mut buf, mut got) = (vec![0u8; 1 << 16], 0u64);
    loop {
        let n = resp.read(&mut buf).map_err(net_err)?;
        if n == 0 {
            break;
        }
        hash.update(&buf[..n]);
        file.write_all(&buf[..n]).map_err(file_err)?;
        got += n as u64;
        progress(got, total);
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
    fn download_keeps_a_file_whose_hash_matches_and_reports_progress() {
        let body: &[u8] = b"model bytes";
        let dest = temp_dir("ok").join("m.hzm");
        let mut last = (0, None);
        let got = download_checked(
            &serve_once(body),
            &hex(&Sha256::digest(body)),
            &dest,
            &mut |n, t| last = (n, t),
        )
        .unwrap();
        assert_eq!(std::fs::read(got).unwrap(), body);
        assert_eq!(last, (body.len() as u64, Some(body.len() as u64)));
    }

    #[test]
    fn download_with_wrong_hash_leaves_nothing() {
        let dir = temp_dir("bad");
        let dest = dir.join("m.hzm");
        let err = download_checked(
            &serve_once(b"tampered"),
            &"0".repeat(64),
            &dest,
            &mut |_, _| {},
        )
        .unwrap_err();
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
    fn both_programs_have_release_builds_named_alike() {
        let v = Version::new(1, 2, 3);
        let cli = asset_name("hamzip", &v).unwrap();
        let gui = asset_name("hamzip-gui", &v).unwrap();
        assert_eq!(gui, cli.replacen("hamzip-", "hamzip-gui-", 1));
    }

    #[test]
    fn installed_programs_finds_what_is_there() {
        let dir = temp_dir("progs");
        std::fs::create_dir_all(&dir).unwrap();
        assert!(installed_programs(&dir).is_empty());
        let gui = dir.join(format!("hamzip-gui{}", std::env::consts::EXE_SUFFIX));
        std::fs::write(&gui, b"x").unwrap();
        assert_eq!(installed_programs(&dir), vec![(gui, "hamzip-gui")]);
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
