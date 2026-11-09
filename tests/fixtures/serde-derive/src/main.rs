use serde::Serialize;

#[derive(Serialize)]
struct Data {
    value: u32,
}

fn main() {
    println!("{}", serde_json::to_string(&Data { value: 42 }).unwrap());
}
