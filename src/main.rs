mod upgrade;

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, Instant};

use clap::{Parser, Subcommand};
use hamzip::{armor, compress, decompress, mode_of, net, store, Error, Mode, Model, Tokenizer};
use sha2::{Digest, Sha256};

#[derive(Parser)]
#[command(
    version,
    about = "Compress short text messages for slow radio links with a shared language model"
)]
struct Cli {
    /// Model both stations share: a published model's name (see `hamzip model list`) or the
    /// path to a model file. Default: smollm2-135m for encoding; decoding uses whichever
    /// installed model the message names
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
    /// Replace this hamzip, and hamzip-gui beside it, with the latest release, after checking their SHA-256
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
    /// Download a published model and check its SHA-256
    Download {
        /// smollm2-135m (137 MB, the default) or smollm2-360m (365 MB, about 9% smaller
        /// messages, half the decode speed)
        name: Option<String>,
        /// Folder to save it in (default: the data folder)
        #[arg(long)]
        to: Option<PathBuf>,
    },
    /// List the published models and where each is installed
    List,
    /// Print the model file hamzip would encode with
    Path,
}

/// The published model `--model` names, if it names one rather than a file.
fn published(given: &Option<PathBuf>) -> Option<&'static str> {
    let s = given.as_ref()?.to_str()?;
    hamzip::MODELS.iter().map(|(n, _)| *n).find(|n| *n == s)
}

/// `--model` set to a file of the user's own, which then always wins.
fn custom_path(given: &Option<PathBuf>) -> bool {
    given.is_some() && published(given).is_none()
}

/// The file hamzip encodes with, and the published model it has to be, if any.
fn model_path(given: &Option<PathBuf>) -> Result<(PathBuf, Option<&'static str>), Error> {
    if let (Some(p), true) = (given, custom_path(given)) {
        return Ok((p.clone(), None));
    }
    let name = published(given).unwrap_or(hamzip::MODELS[0].0);
    store::find_installed(name)
        .map(|p| (p, Some(name)))
        .ok_or_else(|| store::not_installed(name))
}

fn load(given: &Option<PathBuf>) -> Result<Model, Error> {
    let (path, name) = model_path(given)?;
    store::load_checked(&path, name)
}

/// The model to decode a model-mode `msg` with: the published model the message
/// names, so a station can keep both, unless `--model` points at a file of its own.
fn model_for(given: &Option<PathBuf>, msg: &[u8]) -> Result<Model, Error> {
    let wanted = msg
        .get(1..3)
        .and_then(|id| hamzip::model_name([id[0], id[1]]));
    match wanted {
        Some(name) if !custom_path(given) => store::load_published(name),
        _ => load(given),
    }
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
    hamzip::message_from(&read_input(path)?)
}

fn info(msg: &[u8], given: &Option<PathBuf>) -> Result<(), Error> {
    let mode = mode_of(msg)?;
    let framing = mode.framing();
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
        let id = [msg[1], msg[2]];
        println!("coded with model {}", hamzip::describe_model(id));
        match hamzip::model_name(id) {
            Some(name) if !custom_path(given) => match store::find_installed(name) {
                Some(p) => println!("installed here: {}", p.display()),
                None => println!("not installed here; get it with `hamzip model download {name}`"),
            },
            _ if given.is_some() => {
                let (path, name) = model_path(given)?;
                let m = store::load_checked(&path, name)?;
                let verdict = if m.id() == id {
                    "matches"
                } else {
                    "does not match"
                };
                println!("{verdict} {}", path.display());
            }
            _ => {}
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
                // a text channel carries UTF-8, so bytes are what both cost
                if line.len() >= plain.len() {
                    eprintln!(
                        "note: the hz: line ({} bytes) is no shorter than the message ({}); \
                         on a text mode, sending the message as is costs less",
                        line.len(),
                        plain.len()
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
                Mode::Model => Some(model_for(&cli.model, &msg)?),
                _ => None,
            };
            write_output(output, &decompress(m.as_ref(), &msg)?)
        }
        Cmd::Info { input } => info(&read_message(input)?, &cli.model),
        Cmd::Bench { dir, encode_only } => bench(&load(&cli.model)?, dir, *encode_only),
        Cmd::Model {
            cmd: ModelCmd::Download { name, to },
        } => {
            let name = name
                .as_deref()
                .or(published(&cli.model))
                .unwrap_or(hamzip::MODELS[0].0);
            let dir = to.clone().unwrap_or_else(store::data_dir);
            let mut shown = 0;
            let path = net::download_model(name, &dir, &mut |got, total| {
                if got - shown >= 16 << 20 {
                    shown = got;
                    match total {
                        Some(t) => eprintln!("{} of {} MB", got >> 20, t >> 20),
                        None => eprintln!("{} MB", got >> 20),
                    }
                }
            })?;
            println!("saved {}", path.display());
            Ok(())
        }
        Cmd::Model {
            cmd: ModelCmd::List,
        } => {
            for (i, (name, _)) in hamzip::MODELS.iter().enumerate() {
                let default = if i == 0 { " (default)" } else { "" };
                match store::find_installed(name) {
                    Some(p) => println!("{name}{default}: {}", p.display()),
                    None => println!("{name}{default}: not installed"),
                }
            }
            Ok(())
        }
        Cmd::Model {
            cmd: ModelCmd::Path,
        } => {
            println!("{}", model_path(&cli.model)?.0.display());
            Ok(())
        }
        Cmd::Upgrade { check, yes } => upgrade::upgrade(*check, *yes),
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    net::clean_old_exe("hamzip");
    let notice = match cli.cmd {
        Cmd::Upgrade { .. } => None,
        _ => upgrade::start_notice(&store::data_dir()),
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
