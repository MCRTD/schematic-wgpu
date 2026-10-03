//! Compare a .litematic file with its preview payload:
//! `cargo run --release -p schematic-wgpu-litematic --example preview_size -- file.litematic`
use std::time::Instant;

fn main() {
    for path in std::env::args().skip(1) {
        let bytes = std::fs::read(&path).expect("read file");
        let t = Instant::now();
        let lit = schematic_wgpu_litematic::Litematic::from_gzip_bytes(&bytes).expect("parse");
        let parse = t.elapsed();
        let preview = lit.to_preview();
        let t = Instant::now();
        let back = schematic_wgpu_litematic::Litematic::from_preview(&preview).expect("decode preview");
        let decode = t.elapsed();
        assert_eq!(back, lit);
        println!(
            "{path}: litematic {} B ({parse:?}) → preview {} B ({decode:?})",
            bytes.len(),
            preview.len()
        );
    }
}
