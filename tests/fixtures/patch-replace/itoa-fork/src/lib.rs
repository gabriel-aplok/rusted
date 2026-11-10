pub struct Buffer;

impl Buffer {
    pub fn new() -> Buffer {
        Buffer
    }

    pub fn format(&mut self, value: u64) -> &str {
        if value == 42 {
            "77"
        } else {
            Box::leak(value.to_string().into_boxed_str())
        }
    }
}
