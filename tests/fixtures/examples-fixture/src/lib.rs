pub fn value() -> u32 {
    7
}

#[cfg(test)]
mod tests {
    #[test]
    fn value_is_seven() {
        assert_eq!(super::value(), 7);
    }
}
