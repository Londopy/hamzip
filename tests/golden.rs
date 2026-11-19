//! Fixed messages and the exact bytes they must encode to. Any machine, any
//! build and any other implementation has to reproduce these; if one of them
//! changes, messages sent with earlier versions no longer decode. Replace them
//! only together with a new message format version.

use std::path::{Path, PathBuf};

use hamzip::{compress, decompress, Model, Tokenizer};

fn model() -> Model {
    let path = std::env::var_os("HAMZIP_MODEL")
        .map(PathBuf::from)
        .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("models/smollm2-135m.hzm"));
    Model::load(&path).unwrap_or_else(|e| {
        panic!("the golden test needs the model file ({e}); run `hamzip model download` or set HAMZIP_MODEL")
    })
}

#[test]
fn golden_messages_encode_and_decode_exactly() {
    let m = model();
    let tok = Tokenizer::new(&m);
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden");
    let mut texts: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|x| x == "txt"))
        .collect();
    texts.sort();
    assert!(
        texts.len() >= 29,
        "only {} golden messages found",
        texts.len()
    );
    for t in texts {
        let text = std::fs::read(&t).unwrap();
        let want = std::fs::read(t.with_extension("hz")).unwrap();
        let name = t.file_name().unwrap().to_string_lossy();
        assert_eq!(
            compress(Some(&m), Some(&tok), &text),
            want,
            "{name} encodes differently"
        );
        assert_eq!(
            decompress(Some(&m), &want).unwrap(),
            text,
            "{name} decodes differently"
        );
    }
}
