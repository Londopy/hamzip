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

/// The programs an upgrade of `exe` replaces: the others installed beside it,
/// then `exe` itself under its program name `own`, whatever its file is called
/// (a portable download keeps its release name). The running one goes last.
pub fn programs_beside(exe: &Path, own: &'static str) -> Vec<(PathBuf, &'static str)> {
    let mut programs: Vec<_> = exe
        .parent()
        .map(installed_programs)
        .unwrap_or_default()
        .into_iter()
        .filter(|(path, name)| *name != own && path != exe)
        .collect();
    programs.push((exe.to_path_buf(), own));
    programs
}

/// A program's new build, downloaded and checked, ready for `replace_all`.
pub struct Fetched {
    path: PathBuf,
    bytes: Vec<u8>,
}

/// Downloads each program's build from `rel` and checks it against
/// `SHA256SUMS.txt`. Nothing on disk changes.
pub fn fetch(rel: &Release, programs: &[(PathBuf, &str)]) -> Result<Vec<Fetched>, Error> {
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
        fetched.push(Fetched {
            path: path.clone(),
            bytes: bytes.to_vec(),
        });
    }
    Ok(fetched)
}

/// Replaces every program with its fetched build, or none of them. Each new
/// build is written beside its program and started once with `--version` (a
/// build for a newer glibc fails here) before any program is moved aside; the
/// running one, which Windows can rename but not overwrite, keeps working.
pub fn replace_all(fetched: &[Fetched]) -> Result<(), Error> {
    replace_with_probe(fetched, &["--version"])
}

fn replace_with_probe(fetched: &[Fetched], probe: &[&str]) -> Result<(), Error> {
    let staged: Vec<PathBuf> = fetched
        .iter()
        .map(|f| f.path.with_extension("new"))
        .collect();
    let unstage = || {
        for s in &staged {
            let _ = std::fs::remove_file(s);
        }
    };
    for (f, s) in fetched.iter().zip(&staged) {
        if let Err(e) = stage(s, &f.bytes, probe) {
            unstage();
            return Err(Error::Io(format!(
                "could not replace {}: {e}. Nothing was changed",
                f.path.display()
            )));
        }
    }
    let mut swapped: Vec<&Path> = Vec::new();
    for (f, s) in fetched.iter().zip(&staged) {
        let old = old_path(&f.path);
        let _ = std::fs::remove_file(&old);
        let moved = std::fs::rename(&f.path, &old).and_then(|_| {
            std::fs::rename(s, &f.path).inspect_err(|_| {
                let _ = std::fs::rename(&old, &f.path);
            })
        });
        if let Err(e) = moved {
            let mut back = Vec::new();
            for p in swapped.iter().rev() {
                let restored =
                    std::fs::remove_file(p).and_then(|_| std::fs::rename(old_path(p), p));
                if restored.is_err() {
                    back.push(p.display().to_string());
                }
            }
            unstage();
            let changed = if back.is_empty() {
                "Nothing was changed".to_string()
            } else {
                format!("Putting back the old {} failed too", back.join(" and "))
            };
            return Err(Error::Io(format!(
                "could not replace {}: {e}. {changed}",
                f.path.display()
            )));
        }
        swapped.push(&f.path);
    }
    Ok(())
}

/// Writes a new build to `path` and checks that it starts.
fn stage(path: &Path, bytes: &[u8], probe: &[&str]) -> std::io::Result<()> {
    File::create(path).and_then(|mut f| f.write_all(bytes).and_then(|_| f.sync_all()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))?;
    }
    let mut probe_cmd = std::process::Command::new(path);
    probe_cmd.args(probe);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // the window starts this probe; don't flash a console from it
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        probe_cmd.creation_flags(CREATE_NO_WINDOW);
    }
    if probe_cmd.output().is_ok_and(|o| o.status.success()) {
        Ok(())
    } else {
        Err(std::io::Error::other(
            "the downloaded program does not start on this machine",
        ))
    }
}

pub fn old_path(exe: &Path) -> PathBuf {
    exe.with_extension("old")
}

/// Deletes the copies an upgrade left behind, once the new programs are running.
pub fn clean_old_exe() {
    if let Ok(exe) = std::env::current_exe() {
        for (path, _) in programs_beside(&exe, "hamzip") {
            let _ = std::fs::remove_file(old_path(&path));
        }
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

    /// An empty folder that is deleted when the test ends.
    struct TempDir(PathBuf);

    impl std::ops::Deref for TempDir {
        type Target = Path;
        fn deref(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn temp_dir(name: &str) -> TempDir {
        let d = std::env::temp_dir().join(format!("hamzip-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        TempDir(d)
    }

    #[test]
    fn download_keeps_a_file_whose_hash_matches_and_reports_progress() {
        let body: &[u8] = b"model bytes";
        let dir = temp_dir("ok");
        let dest = dir.join("m.hzm");
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
        assert!(installed_programs(&dir).is_empty());
        let gui = dir.join(format!("hamzip-gui{}", std::env::consts::EXE_SUFFIX));
        std::fs::write(&gui, b"x").unwrap();
        assert_eq!(installed_programs(&dir), vec![(gui, "hamzip-gui")]);
    }

    #[test]
    fn a_renamed_portable_download_is_upgraded_last_under_its_own_name() {
        let dir = temp_dir("beside");
        let exe = |n: &str| dir.join(format!("{n}{}", std::env::consts::EXE_SUFFIX));
        std::fs::write(exe("hamzip"), b"x").unwrap();
        let portable = exe("hamzip-gui-v0.2.0-x64-portable");
        std::fs::write(&portable, b"x").unwrap();
        assert_eq!(
            programs_beside(&portable, "hamzip-gui"),
            vec![(exe("hamzip"), "hamzip"), (portable, "hamzip-gui")]
        );
        // a second copy of the running program's own kind is left alone
        std::fs::write(exe("hamzip-gui"), b"x").unwrap();
        assert_eq!(
            programs_beside(&exe("hamzip"), "hamzip"),
            vec![(exe("hamzip-gui"), "hamzip-gui"), (exe("hamzip"), "hamzip")]
        );
    }

    // serves each path in `files` once, standing in for a GitHub release
    fn serve_release(files: Vec<(String, Vec<u8>)>) -> Release {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let assets = files
            .iter()
            .map(|(name, _)| Asset {
                name: name.clone(),
                browser_download_url: format!("{base}/{name}"),
            })
            .collect();
        std::thread::spawn(move || {
            for _ in 0..files.len() {
                let (mut s, _) = listener.accept().unwrap();
                let mut r = BufReader::new(s.try_clone().unwrap());
                let mut line = String::new();
                r.read_line(&mut line).unwrap();
                let path = line.split(' ').nth(1).unwrap_or("").trim_start_matches('/');
                let body = files
                    .iter()
                    .find(|(n, _)| n == path)
                    .map(|(_, b)| b.clone())
                    .unwrap();
                let mut rest = String::new();
                while r.read_line(&mut rest).unwrap() > 2 {
                    rest.clear();
                }
                let head = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                s.write_all(head.as_bytes()).unwrap();
                s.write_all(&body).unwrap();
            }
        });
        Release {
            tag_name: "v9.9.9".into(),
            html_url: format!("{base}/release"),
            assets,
        }
    }

    #[test]
    fn fetch_checks_every_build_against_the_sums() {
        let ver = Version::new(9, 9, 9);
        let (cli, gui) = (
            asset_name("hamzip", &ver).unwrap(),
            asset_name("hamzip-gui", &ver).unwrap(),
        );
        let programs = [
            (PathBuf::from("hamzip"), "hamzip"),
            (PathBuf::from("hamzip-gui"), "hamzip-gui"),
        ];
        let sums = |gui_bytes: &[u8]| {
            format!(
                "{}  {cli}\n{}  {gui}\n",
                hex(&Sha256::digest(b"cli")),
                hex(&Sha256::digest(gui_bytes))
            )
            .into_bytes()
        };
        let release = |sums: Vec<u8>| {
            serve_release(vec![
                ("SHA256SUMS.txt".into(), sums),
                (cli.clone(), b"cli".to_vec()),
                (gui.clone(), b"gui".to_vec()),
            ])
        };
        let got = fetch(&release(sums(b"gui")), &programs).unwrap();
        assert_eq!(got.len(), 2);
        assert_eq!(
            (got[1].path.as_path(), got[1].bytes.as_slice()),
            (Path::new("hamzip-gui"), &b"gui"[..])
        );
        let err = fetch(&release(sums(b"tampered")), &programs)
            .err()
            .unwrap()
            .to_string();
        assert!(err.contains(&gui) && err.contains("SHA-256"), "{err}");
    }

    // this test binary is a real program; `--list` makes it exit 0
    fn runnable() -> Vec<u8> {
        std::fs::read(std::env::current_exe().unwrap()).unwrap()
    }

    #[test]
    fn replace_all_swaps_in_every_program() {
        let dir = temp_dir("swap");
        let (cli, gui) = (dir.join("hamzip.exe"), dir.join("hamzip-gui.exe"));
        std::fs::write(&cli, b"old cli").unwrap();
        std::fs::write(&gui, b"old gui").unwrap();
        let new = runnable();
        let fetched = [&cli, &gui].map(|p| Fetched {
            path: p.clone(),
            bytes: new.clone(),
        });
        replace_with_probe(&fetched, &["--list"]).unwrap();
        for (p, old) in [(&cli, &b"old cli"[..]), (&gui, b"old gui")] {
            assert_eq!(std::fs::read(p).unwrap(), new);
            assert_eq!(std::fs::read(old_path(p)).unwrap(), old);
            assert!(!p.with_extension("new").exists());
        }
    }

    #[test]
    fn replace_all_changes_nothing_when_one_build_does_not_start() {
        let dir = temp_dir("junk");
        let (cli, gui) = (dir.join("hamzip.exe"), dir.join("hamzip-gui.exe"));
        std::fs::write(&cli, b"old cli").unwrap();
        std::fs::write(&gui, b"old gui").unwrap();
        let fetched = [
            Fetched {
                path: cli.clone(),
                bytes: runnable(),
            },
            Fetched {
                path: gui.clone(),
                bytes: b"not a program".to_vec(),
            },
        ];
        let err = replace_with_probe(&fetched, &["--list"])
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("does not start") && err.contains("Nothing was changed"),
            "{err}"
        );
        for (p, old) in [(&cli, &b"old cli"[..]), (&gui, b"old gui")] {
            assert_eq!(std::fs::read(p).unwrap(), old);
            assert!(!p.with_extension("new").exists() && !old_path(p).exists());
        }
    }
}
