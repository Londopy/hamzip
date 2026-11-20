mod upgrade;

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, Instant};

use clap::{Parser, Subcommand};
use hamzip::{armor, compress, decompress, mode_of, Error, Mode, Model, Tokenizer};
use sha2::{Digest, Sha256};

#[derive(Parser)]
#[command(
    version,
    about = "Compress short text messages for slow radio links with a shared language model"
)]
struct Cli {
    /// Model file both stations share. Default: smollm2-135m.hzm next to hamzip, then in
    /// the data folder (`hamzip model download` puts it there)
    #[arg(short, long, env = "HAMZIP_MODEL", global = true)]
    model: Option<PathBuf>,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Compress text (file or stdin) to a binary message
    Encode {
        input: Option<PathBuf>,
        #[arg(short, long)]
        output: Option<PathBuf>,
        /// Write the message as `hz:` text (base64) for modes that only carry text
        #[arg(long)]
        text: bool,
    },
    /// Turn a message, binary or `hz:` text, back into text
    Decode {
        input: Option<PathBuf>,
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
    /// Show how a message was coded, without decoding it
    Info { input: Option<PathBuf> },
    /// Round-trip every file in a directory and report size, speed and a digest of the output
    Bench {
        dir: PathBuf,
        /// Only encode, for a quick digest on a slow machine
        #[arg(long)]
        encode_only: bool,
    },
    /// Get the shared model file
    Model {
        #[command(subcommand)]
        cmd: ModelCmd,
    },
    /// Replace this hamzip with the latest release, after checking its SHA-256
    Upgrade {
        /// Only say whether a newer version exists
        #[arg(long)]
        check: bool,
        /// Don't ask before replacing the executable
        #[arg(long)]
        yes: bool,
    },
}

#[derive(Subcommand)]
enum ModelCmd {
    /// Download smollm2-135m.hzm (137 MB) and check its SHA-256
    Download {
        /// Folder to save it in (default: the data folder)
        #[arg(long)]
        to: Option<PathBuf>,
    },
    /// Print the model file hamzip would use
    Path,
}

fn exe_dir() -> Option<PathBuf> {
    Some(std::env::current_exe().ok()?.parent()?.to_path_buf())
}

/// Per-user data folder; a `portable` file next to the exe keeps everything beside it.
fn data_dir() -> PathBuf {
    if let Some(d) = exe_dir().filter(|d| d.join("portable").exists()) {
        return d;
    }
    let base = if cfg!(windows) {
        std::env::var_os("LOCALAPPDATA").map(PathBuf::from)
    } else {
        std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| Path::new(&h).join(".local/share")))
    };
    base.unwrap_or_else(|| PathBuf::from(".")).join("hamzip")
}

fn model_path(given: &Option<PathBuf>) -> Result<PathBuf, Error> {
    if let Some(p) = given {
        return Ok(p.clone());
    }
    exe_dir()
        .into_iter()
        .chain([data_dir()])
        .map(|d| d.join(upgrade::MODEL_FILE))
        .find(|p| p.is_file())
        .ok_or_else(|| {
            Error::Io(
                "no model found: run `hamzip model download`, pass --model, or set HAMZIP_MODEL"
                    .into(),
            )
        })
}

fn load(given: &Option<PathBuf>) -> Result<Model, Error> {
    Model::load(&model_path(given)?)
}

fn read_input(path: &Option<PathBuf>) -> Result<Vec<u8>, Error> {
    match path {
        Some(p) => std::fs::read(p).map_err(|e| Error::Io(format!("{}: {e}", p.display()))),
        None => {
            let mut buf = Vec::new();
            std::io::stdin()
                .read_to_end(&mut buf)
                .map_err(|e| Error::Io(format!("stdin: {e}")))?;
            Ok(buf)
        }
    }
}

fn write_output(path: &Option<PathBuf>, data: &[u8]) -> Result<(), Error> {
    match path {
        Some(p) => std::fs::write(p, data).map_err(|e| Error::Io(format!("{}: {e}", p.display()))),
        None => std::io::stdout()
            .write_all(data)
            .map_err(|e| Error::Io(format!("stdout: {e}"))),
    }
}

/// A message from a file or stdin, binary or in `hz:` text form.
fn read_message(path: &Option<PathBuf>) -> Result<Vec<u8>, Error> {
    let raw = read_input(path)?;
    match armor::from_text(&raw) {
        Some(msg) => msg.map_err(|e| Error::BadMessage(format!("hz: text: {e}"))),
        None => Ok(raw),
    }
}

fn info(msg: &[u8], given: &Option<PathBuf>) -> Result<(), Error> {
    let mode = mode_of(msg)?;
    let framing = if mode == Mode::Model { 7 } else { 5 };
    let name = format!("{mode:?}").to_lowercase();
    if msg.len() < framing {
        return Err(Error::BadMessage(format!(
            "{} bytes is too short for a {name}-mode message, which has {framing} bytes of framing",
            msg.len()
        )));
    }
    println!(
        "format 1, {name} mode, {} bytes ({framing} of them framing)",
        msg.len()
    );
    if mode == Mode::Model {
        let id = format!("{:02x}{:02x}", msg[1], msg[2]);
        let Ok(path) = model_path(given) else {
            println!("coded with model {id}..; no model here to compare");
            return Ok(());
        };
        let m = Model::load(&path)?;
        if m.id() == [msg[1], msg[2]] {
            println!("coded with model {id}.., which is {}", path.display());
        } else {
            println!("coded with model {id}.., not {}", path.display());
        }
    }
    Ok(())
}

fn bench(m: &Model, dir: &Path, encode_only: bool) -> Result<(), Error> {
    let tok = Tokenizer::new(m);
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .map_err(|e| Error::Io(format!("{}: {e}", dir.display())))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.is_file())
        .collect();
    files.sort();
    if files.is_empty() {
        return Err(Error::Io(format!(
            "{}: no files to benchmark",
            dir.display()
        )));
    }

    let (mut chars, mut bytes, mut tokens) = (0usize, 0usize, 0usize);
    let (mut enc_s, mut dec_s) = (0f64, 0f64);
    let mut per_msg = Vec::new();
    // two machines that print the same digest for the same files encode identically
    let mut digest = Sha256::new();
    println!("file\tchars\ttokens\tbytes\tmode\tenc_ms\tdec_ms");
    for f in &files {
        let text = std::fs::read(f).map_err(|e| Error::Io(format!("{}: {e}", f.display())))?;
        let n = String::from_utf8_lossy(&text).chars().count();
        let ntok = std::str::from_utf8(&text)
            .ok()
            .and_then(|s| tok.encode(s))
            .map_or(0, |t| t.len());
        let t0 = Instant::now();
        let msg = compress(Some(m), Some(&tok), &text);
        let t1 = Instant::now();
        if !encode_only && decompress(Some(m), &msg)? != text {
            return Err(Error::BadMessage(format!(
                "{} did not round-trip",
                f.display()
            )));
        }
        let t2 = Instant::now();
        let mode = mode_of(&msg)?;
        // zstd output depends on the zstd version, which the format doesn't pin
        if mode == Mode::Model {
            digest.update((msg.len() as u32).to_le_bytes());
            digest.update(&msg);
            tokens += ntok + 1;
            enc_s += (t1 - t0).as_secs_f64();
            dec_s += (t2 - t1).as_secs_f64();
        }
        chars += n.max(1);
        bytes += msg.len();
        per_msg.push(msg.len() as f64 * 8.0 / n.max(1) as f64);
        let name = f.file_name().unwrap().to_string_lossy();
        println!(
            "{name}\t{n}\t{ntok}\t{}\t{mode:?}\t{:.0}\t{:.0}",
            msg.len(),
            (t1 - t0).as_secs_f64() * 1e3,
            (t2 - t1).as_secs_f64() * 1e3
        );
    }
    per_msg.sort_by(f64::total_cmp);
    let mid = per_msg.len() / 2;
    let median = if per_msg.len() % 2 == 0 {
        (per_msg[mid - 1] + per_msg[mid]) / 2.0
    } else {
        per_msg[mid]
    };
    println!(
        "\n{} messages {}. {:.3} bpc overall, {median:.3} median, headers included",
        files.len(),
        if encode_only {
            "encoded"
        } else {
            "round-tripped"
        },
        bytes as f64 * 8.0 / chars as f64,
    );
    if tokens > 0 {
        let decode = if encode_only {
            String::new()
        } else {
            format!(", decode {:.0} tok/s", tokens as f64 / dec_s)
        };
        println!(
            "model-coded tokens: {tokens}, encode {:.0} tok/s{decode}",
            tokens as f64 / enc_s
        );
    }
    let hex: String = digest
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    println!("digest {hex}");
    Ok(())
}

fn run(cli: &Cli) -> Result<(), Error> {
    match &cli.cmd {
        Cmd::Encode {
            input,
            output,
            text,
        } => {
            let m = load(&cli.model)?;
            let tok = Tokenizer::new(&m);
            let plain = read_input(input)?;
            let msg = compress(Some(&m), Some(&tok), &plain);
            if *text {
                let line = armor::to_text(&msg);
                let chars = String::from_utf8_lossy(&plain).chars().count();
                if line.len() >= chars {
                    eprintln!(
                        "note: the hz: line ({} characters) is no shorter than the message ({chars}); \
                         on a text mode, sending the message as is costs less",
                        line.len()
                    );
                }
                write_output(output, format!("{line}\n").as_bytes())
            } else {
                write_output(output, &msg)
            }
        }
        Cmd::Decode { input, output } => {
            let msg = read_message(input)?;
            // zstd and raw messages decode without loading the model
            let m = match mode_of(&msg)? {
                Mode::Model => Some(load(&cli.model)?),
                _ => None,
            };
            write_output(output, &decompress(m.as_ref(), &msg)?)
        }
        Cmd::Info { input } => info(&read_message(input)?, &cli.model),
        Cmd::Bench { dir, encode_only } => bench(&load(&cli.model)?, dir, *encode_only),
        Cmd::Model {
            cmd: ModelCmd::Download { to },
        } => {
            let path = upgrade::download_model(&to.clone().unwrap_or_else(data_dir))?;
            println!("saved {}", path.display());
            Ok(())
        }
        Cmd::Model {
            cmd: ModelCmd::Path,
        } => {
            println!("{}", model_path(&cli.model)?.display());
            Ok(())
        }
        Cmd::Upgrade { check, yes } => upgrade::upgrade(*check, *yes),
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    upgrade::clean_old_exe();
    let notice = match cli.cmd {
        Cmd::Upgrade { .. } => None,
        _ => upgrade::start_notice(&data_dir()),
    };
    let result = run(&cli);
    // the check runs alongside the command; don't hold the user up waiting for it
    if let Some(msg) = notice.and_then(|rx| rx.recv_timeout(Duration::from_millis(300)).ok()) {
        eprintln!("{msg}");
    }
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("hamzip: {e}");
            ExitCode::FAILURE
        }
    }
}
