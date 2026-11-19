use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Instant;

use clap::{Parser, Subcommand};
use hamzip::{compress, decompress, mode_of, Error, Mode, Model, Tokenizer};

#[derive(Parser)]
#[command(
    version,
    about = "Compress short text messages for slow radio links with a shared language model"
)]
struct Cli {
    /// Model file both stations share
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
    },
    /// Turn a binary message back into text
    Decode {
        input: Option<PathBuf>,
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
    /// Round-trip every file in a directory and report size and speed
    Bench { dir: PathBuf },
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

fn load(model: &Option<PathBuf>) -> Result<Model, Error> {
    let path = model
        .as_ref()
        .ok_or_else(|| Error::Io("no model: pass --model or set HAMZIP_MODEL".into()))?;
    Model::load(path)
}

fn bench(m: &Model, dir: &Path) -> Result<(), Error> {
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
        let back = decompress(Some(m), &msg)?;
        let t2 = Instant::now();
        if back != text {
            return Err(Error::BadMessage(format!(
                "{} did not round-trip",
                f.display()
            )));
        }
        let mode = mode_of(&msg)?;
        if mode == Mode::Model {
            tokens += ntok + 1;
            enc_s += (t1 - t0).as_secs_f64();
            dec_s += (t2 - t1).as_secs_f64();
        }
        chars += n;
        bytes += msg.len();
        per_msg.push(msg.len() as f64 * 8.0 / n as f64);
        let name = f.file_name().unwrap().to_string_lossy();
        println!(
            "{name}\t{n}\t{ntok}\t{}\t{mode:?}\t{:.0}\t{:.0}",
            msg.len(),
            (t1 - t0).as_secs_f64() * 1e3,
            (t2 - t1).as_secs_f64() * 1e3
        );
    }
    per_msg.sort_by(f64::total_cmp);
    println!(
        "\n{} messages round-tripped. {:.3} bpc overall, {:.3} median, headers included",
        files.len(),
        bytes as f64 * 8.0 / chars as f64,
        per_msg[per_msg.len() / 2]
    );
    if tokens > 0 {
        println!(
            "model-coded tokens: {tokens}, encode {:.0} tok/s, decode {:.0} tok/s",
            tokens as f64 / enc_s,
            tokens as f64 / dec_s
        );
    }
    Ok(())
}

fn run(cli: Cli) -> Result<(), Error> {
    match &cli.cmd {
        Cmd::Encode { input, output } => {
            let m = load(&cli.model)?;
            let tok = Tokenizer::new(&m);
            let text = read_input(input)?;
            write_output(output, &compress(Some(&m), Some(&tok), &text))
        }
        Cmd::Decode { input, output } => {
            let msg = read_input(input)?;
            // zstd and raw messages decode without loading the model
            let m = match mode_of(&msg)? {
                Mode::Model => Some(load(&cli.model)?),
                _ => None,
            };
            write_output(output, &decompress(m.as_ref(), &msg)?)
        }
        Cmd::Bench { dir } => bench(&load(&cli.model)?, dir),
    }
}

fn main() -> ExitCode {
    match run(Cli::parse()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("hamzip: {e}");
            ExitCode::FAILURE
        }
    }
}
