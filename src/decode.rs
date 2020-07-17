use crate::ast::Symbol;
use hound;

fn decode(filename: &str) -> hound::Result<Symbol> {
    let reader = hound::WavReader::open(filename)?;
    println!("duration: {0}", reader.duration());
    panic!();
}
