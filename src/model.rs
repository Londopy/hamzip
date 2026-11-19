use std::collections::HashMap;
use std::path::Path;

use sha2::{Digest, Sha256};

use crate::Error;

const MAGIC: &[u8; 8] = b"HAMZIPML";
const VERSION: u32 = 1;

pub struct Config {
    pub n_layers: usize,
    pub d_model: usize,
    pub n_heads: usize,
    pub n_kv_heads: usize,
    pub head_dim: usize,
    pub d_ff: usize,
    pub vocab: usize,
    pub max_ctx: usize,
    pub eos: u32,
}

/// An int8 matrix with one scale per row: `w = q * mant * 2^-shift`.
pub struct Linear {
    pub rows: usize,
    pub cols: usize,
    pub q: Vec<i8>,
    pub mant: Vec<i64>,
    pub shift: Vec<i32>,
}

impl Linear {
    // rows are independent, so stacking matrices that share an input changes no result
    fn stack(parts: Vec<Linear>) -> Linear {
        let cols = parts[0].cols;
        let mut out = Linear {
            rows: 0,
            cols,
            q: Vec::new(),
            mant: Vec::new(),
            shift: Vec::new(),
        };
        for p in parts {
            out.rows += p.rows;
            out.q.extend(p.q);
            out.mant.extend(p.mant);
            out.shift.extend(p.shift);
        }
        out
    }
}

pub struct Layer {
    pub attn_norm: Vec<i32>,
    pub mlp_norm: Vec<i32>,
    /// q, k and v rows stacked
    pub qkv: Linear,
    pub o: Linear,
    /// gate and up rows stacked
    pub gate_up: Linear,
    pub down: Linear,
}

pub struct Model {
    pub cfg: Config,
    pub embed: Linear,
    pub layers: Vec<Layer>,
    pub final_norm: Vec<i32>,
    pub silu: Vec<i32>,
    pub exp: Vec<u32>,
    pub rope_cos: Vec<i32>,
    pub rope_sin: Vec<i32>,
    pub tok_offsets: Vec<u32>,
    pub tok_bytes: Vec<u8>,
    pub merges: Vec<[u32; 3]>,
    pub sha256: [u8; 32],
}

struct Section<'a> {
    dtype: u32,
    shape: Vec<usize>,
    data: &'a [u8],
}

struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], Error> {
        let end = self.pos.checked_add(n).filter(|&e| e <= self.buf.len());
        let end = end.ok_or_else(|| bad("file is truncated"))?;
        let s = &self.buf[self.pos..end];
        self.pos = end;
        Ok(s)
    }

    fn u16(&mut self) -> Result<u16, Error> {
        Ok(u16::from_le_bytes(self.take(2)?.try_into().unwrap()))
    }

    fn u32(&mut self) -> Result<u32, Error> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
}

fn bad(msg: impl Into<String>) -> Error {
    Error::BadModel(msg.into())
}

fn elem_size(dtype: u32) -> Result<usize, Error> {
    match dtype {
        0 | 1 => Ok(1),
        2 | 3 => Ok(4),
        d => Err(bad(format!("unknown dtype {d}"))),
    }
}

fn parse(buf: &[u8]) -> Result<HashMap<String, Section<'_>>, Error> {
    let mut r = Reader { buf, pos: 0 };
    if r.take(8)? != MAGIC {
        return Err(bad("not a hamzip model file"));
    }
    let version = r.u32()?;
    if version != VERSION {
        return Err(bad(format!(
            "model format {version}, this hamzip reads {VERSION}"
        )));
    }
    let count = r.u32()?;
    let mut out = HashMap::new();
    for _ in 0..count {
        let len = r.u16()? as usize;
        let name = String::from_utf8(r.take(len)?.to_vec()).map_err(|_| bad("bad section name"))?;
        let dtype = r.u32()?;
        let ndim = r.u32()?;
        let mut shape = Vec::new();
        for _ in 0..ndim {
            shape.push(r.u32()? as usize);
        }
        let size = shape
            .iter()
            .try_fold(elem_size(dtype)?, |a, &d| a.checked_mul(d))
            .ok_or_else(|| bad(format!("section {name} is too large")))?;
        let data = r.take(size)?;
        out.insert(name, Section { dtype, shape, data });
    }
    if r.pos != buf.len() {
        return Err(bad("trailing bytes after last section"));
    }
    Ok(out)
}

struct Sections<'a>(HashMap<String, Section<'a>>);

impl<'a> Sections<'a> {
    fn get(&self, name: &str, dtype: u32, shape: &[usize]) -> Result<&'a [u8], Error> {
        let s = self
            .0
            .get(name)
            .ok_or_else(|| bad(format!("missing section {name}")))?;
        if s.dtype != dtype || (!shape.is_empty() && s.shape != shape) {
            return Err(bad(format!(
                "section {name} has dtype {} shape {:?}, expected dtype {dtype} shape {shape:?}",
                s.dtype, s.shape
            )));
        }
        Ok(s.data)
    }

    fn u32s(&self, name: &str, shape: &[usize]) -> Result<Vec<u32>, Error> {
        let (words, _) = self.get(name, 3, shape)?.as_chunks::<4>();
        Ok(words.iter().map(|&w| u32::from_le_bytes(w)).collect())
    }

    fn i32s(&self, name: &str, shape: &[usize]) -> Result<Vec<i32>, Error> {
        let (words, _) = self.get(name, 2, shape)?.as_chunks::<4>();
        Ok(words.iter().map(|&w| i32::from_le_bytes(w)).collect())
    }

    fn linear(&self, name: &str, rows: usize, cols: usize) -> Result<Linear, Error> {
        let q = self
            .get(name, 1, &[rows, cols])?
            .iter()
            .map(|&b| b as i8)
            .collect();
        let scale = self.u32s(&format!("{name}.scale"), &[rows])?;
        let shift: Vec<i32> = scale.iter().map(|&s| (s >> 24) as i32).collect();
        // keeps acc * mant inside i64 and every rounding shift under 64 (docs/engine.md)
        if cols > 1 << 16 || shift.iter().any(|&e| e > 47) {
            return Err(bad(format!(
                "{name}: too wide or a row scale too small for the engine"
            )));
        }
        Ok(Linear {
            rows,
            cols,
            q,
            mant: scale.iter().map(|&s| (s & 0xFF_FFFF) as i64).collect(),
            shift,
        })
    }

    fn gain(&self, name: &str, d: usize) -> Result<Vec<i32>, Error> {
        let g = self.i32s(name, &[d])?;
        if g.iter().any(|v| v.unsigned_abs() > 1 << 20) {
            return Err(bad(format!("{name}: norm gain above 16")));
        }
        Ok(g)
    }
}

impl Model {
    pub fn load(path: &Path) -> Result<Model, Error> {
        let buf = std::fs::read(path).map_err(|e| Error::Io(format!("{}: {e}", path.display())))?;
        let sha256: [u8; 32] = Sha256::digest(&buf).into();
        let s = Sections(parse(&buf)?);

        let c = s.u32s("config", &[9])?;
        let cfg = Config {
            n_layers: c[0] as usize,
            d_model: c[1] as usize,
            n_heads: c[2] as usize,
            n_kv_heads: c[3] as usize,
            head_dim: c[4] as usize,
            d_ff: c[5] as usize,
            vocab: c[6] as usize,
            max_ctx: c[7] as usize,
            eos: c[8],
        };
        let (d, hd, ff) = (cfg.d_model, cfg.head_dim, cfg.d_ff);
        if d == 0
            || cfg.vocab == 0
            || cfg.vocab >= 1 << 23
            || cfg.max_ctx == 0
            || cfg.n_kv_heads == 0
            || cfg.eos as usize >= cfg.vocab
            || cfg.n_heads.checked_mul(hd) != Some(d)
            || !cfg.n_heads.is_multiple_of(cfg.n_kv_heads)
            || !hd.is_multiple_of(2)
        {
            return Err(bad("unsupported model shape"));
        }

        let mut layers = Vec::new();
        for i in 0..cfg.n_layers {
            let kv = cfg.n_kv_heads * hd;
            layers.push(Layer {
                attn_norm: s.gain(&format!("l{i}.attn_norm"), d)?,
                mlp_norm: s.gain(&format!("l{i}.mlp_norm"), d)?,
                qkv: Linear::stack(vec![
                    s.linear(&format!("l{i}.q"), d, d)?,
                    s.linear(&format!("l{i}.k"), kv, d)?,
                    s.linear(&format!("l{i}.v"), kv, d)?,
                ]),
                o: s.linear(&format!("l{i}.o"), d, d)?,
                gate_up: Linear::stack(vec![
                    s.linear(&format!("l{i}.gate"), ff, d)?,
                    s.linear(&format!("l{i}.up"), ff, d)?,
                ]),
                down: s.linear(&format!("l{i}.down"), d, ff)?,
            });
        }

        let tok_offsets = s.u32s("tok.offsets", &[cfg.vocab + 1])?;
        let tok_bytes = s.get("tok.bytes", 0, &[])?.to_vec();
        if tok_offsets.windows(2).any(|w| w[0] > w[1])
            || *tok_offsets.last().unwrap() as usize != tok_bytes.len()
        {
            return Err(bad("token offsets do not match token bytes"));
        }
        let merges = s.u32s("tok.merges", &[])?;
        if !merges.len().is_multiple_of(3) || merges.iter().any(|&t| t as usize >= cfg.vocab) {
            return Err(bad("bad merge table"));
        }

        let half = hd / 2;
        let exp = s.u32s("tbl.exp", &[1537])?;
        if exp[0] != 1 << 30 || exp.windows(2).any(|w| w[0] < w[1]) {
            return Err(bad("tbl.exp must start at 2^30 and never increase"));
        }
        let rope_cos = s.i32s("tbl.rope_cos", &[cfg.max_ctx, half])?;
        let rope_sin = s.i32s("tbl.rope_sin", &[cfg.max_ctx, half])?;
        if rope_cos
            .iter()
            .chain(&rope_sin)
            .any(|v| v.unsigned_abs() > 1 << 30)
        {
            return Err(bad("rope table entry above 1.0"));
        }
        Ok(Model {
            embed: s.linear("embed", cfg.vocab, d)?,
            layers,
            final_norm: s.gain("final_norm", d)?,
            silu: s.i32s("tbl.silu", &[2049])?,
            exp,
            rope_cos,
            rope_sin,
            tok_offsets,
            tok_bytes,
            merges: merges.as_chunks::<3>().0.to_vec(),
            sha256,
            cfg,
        })
    }

    pub fn token_bytes(&self, id: u32) -> &[u8] {
        let i = id as usize;
        &self.tok_bytes[self.tok_offsets[i] as usize..self.tok_offsets[i + 1] as usize]
    }

    /// The two bytes of the model's SHA-256 that every message carries.
    pub fn id(&self) -> [u8; 2] {
        [self.sha256[0], self.sha256[1]]
    }
}
