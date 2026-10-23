use std::{env, path::PathBuf};
fn main() {
    let mut args = env::args().skip(1);
    let root = PathBuf::from(args.next().expect("output directory"));
    let content = args.next().expect("fixture content");
    tinyjuice::cache::configure(256, 1024 * 1024, Some(604800));
    tinyjuice::cache::configure_disk_cap(Some(1024 * 1024));
    tinyjuice::cache::enable_disk_tier(root);
    let (token, retained) = tinyjuice::cache::offload_checked(&content);
    assert!(retained, "released CCR writer must retain its original");
    println!("{token}");
}
