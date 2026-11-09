pub fn answer() -> u32 {
    util::value() + 35
}

#[cfg(test)]
mod tests {
    #[test]
    fn answer_is_42() {
        assert_eq!(super::answer(), 42);
    }
}
