use std::collections::HashMap;

/// Counts words in a sentence.
fn count(text: &str) -> HashMap<&str, usize> {
    let mut counts = HashMap::new();
    for word in text.split_whitespace() {
        *counts.entry(word).or_insert(0) += 1;
    }
    counts
}

fn main() {
    let counts = count("the quick brown fox jumps over the lazy dog the end");
    println!("{counts:?}"); // 42
}
