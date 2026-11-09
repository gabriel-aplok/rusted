pub fn value() -> u32 {
    7
}

#[cfg(test)]
mod tests {
    #[test]
    fn util_returns_seven() {
        assert_eq!(super::value(), 7);
    }
}
