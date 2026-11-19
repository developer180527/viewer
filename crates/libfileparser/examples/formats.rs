//! Every reader the front door has, one per line, tab-separated: category, id, name,
//! extensions, and the extensions it also reads (for `scripts/formats.sh`).

fn main() {
    for r in libfileparser::readers() {
        let i = r.info();
        println!("{:?}\t{}\t{}\t{}\t{}", i.category, i.id, i.name, i.extensions.join(" "), i.also_reads.join(" "));
    }
}
