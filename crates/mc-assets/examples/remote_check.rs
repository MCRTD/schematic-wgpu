//! Check the range-request reader against a local jar by simulating each
//! HTTP range with a file read, and compare with the regular zip reader.
//!
//! cargo run --release -p mc-assets --example remote_check -- client.jar

use std::io::{Read, Seek, SeekFrom};

use mc_assets::remote_zip::{self, TAIL_LEN};
use mc_assets::{AssetSource, ZipSource};

fn main() {
    let path = std::env::args().nth(1).expect("usage: remote_check <client.jar>");
    let mut file = std::fs::File::open(&path).unwrap();
    let len = file.metadata().unwrap().len();
    let (mut requests, mut bytes) = (0, 0);
    let mut fetch = |range: std::ops::Range<u64>| {
        requests += 1;
        bytes += range.end - range.start;
        let mut buf = vec![0; (range.end - range.start) as usize];
        file.seek(SeekFrom::Start(range.start)).unwrap();
        file.read_exact(&mut buf).unwrap();
        buf
    };

    let cd = remote_zip::locate_central_directory(&fetch(len.saturating_sub(TAIL_LEN)..len), len).unwrap();
    let entries = remote_zip::parse_central_directory(&fetch(cd.clone())).unwrap();
    let spans = remote_zip::plan(&entries, cd.start, 64 * 1024, remote_zip::is_block_asset);
    let files: Vec<_> = spans.iter().flat_map(|s| remote_zip::extract(s, &fetch(s.range.clone())).unwrap()).collect();
    println!("{} files via {requests} requests, {:.2} MB of {:.2} MB", files.len(), bytes as f64 / 1e6, len as f64 / 1e6);

    let reference = ZipSource::open(&path).unwrap();
    let mismatches = files.iter().filter(|(name, data)| reference.read(name).as_ref() != Some(data)).count();
    println!("mismatches against the zip crate: {mismatches}");
}
