//! Pure bounds used by the interactive typo-correction path.

pub fn search_prefix(name: &str) -> &str {
    let end = name.char_indices().nth(3).map_or(name.len(), |(index, _)| index);
    &name[..end]
}

pub fn chain(previous: &[String], name: &str) -> Result<Vec<String>, String> {
    let mut chain = previous.to_vec();
    if chain.is_empty() { chain.push(name.to_string()); }
    if chain.len() >= 3 {
        return Err(format!("Recursion depth exceeded for package '{}'. Attempted corrections: {}",
            chain[0], chain.join(" -> ")));
    }
    Ok(chain)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn prefixes_use_characters_not_bytes() {
        assert_eq!(search_prefix("éé"), "éé");
        assert_eq!(search_prefix("a日🦀z"), "a日🦀");
        assert_eq!(search_prefix("firefox"), "fir");
        assert_eq!(search_prefix(""), "");
    }
    #[test]
    fn third_failed_name_stops_the_chain() {
        let mut first = chain(&[], "first").unwrap();
        first.push("second".into());
        let mut second = chain(&first, "second").unwrap();
        second.push("third".into());
        let error = chain(&second, "third").unwrap_err();
        assert!(error.contains("first -> second -> third"));
        assert!(!error.contains("third -> third"));
    }
}
