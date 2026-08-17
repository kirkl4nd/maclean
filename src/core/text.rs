/// "1 item", "3 items". Plain English beats "item(s)" everywhere it appears.
pub fn plural(count: usize, word: &str) -> String {
    if count == 1 {
        format!("1 {word}")
    } else {
        format!("{count} {word}s")
    }
}
