fn main() {
    let found = castavox_core::mirror::find(std::time::Duration::from_secs(6)).unwrap();
    println!("found {} peer(s)", found.len());
    for p in found {
        println!("  id={} name={} product={} addr={:?} port={}", p.id, p.name, p.product, p.address(), p.port);
    }
}
