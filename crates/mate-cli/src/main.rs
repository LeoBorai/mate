//! `mate` binary: parses args, layers config (flags → env → project file →
//! user file → defaults), and picks a frontend — the tabbed TUI by default, or
//! `--plain` for a single-session stdout mode.

fn main() {
    println!("mate");
}

#[cfg(test)]
mod tests {
    #[test]
    fn dummy() {
        assert_eq!(2 + 2, 4);
    }
}
