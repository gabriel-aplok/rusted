use anyhow::Result;
use serde::Serialize;

#[derive(thiserror::Error, Debug)]
enum CustomError {
    #[error("bad value")]
    Bad,
}

#[derive(Serialize)]
struct Data {
    value: u32,
}

fn main() -> Result<()> {
    let data = Data { value: 42 };
    println!("{}", serde_json::to_string(&data)?);
    println!("{}", syn::parse_str::<syn::Expr>("1 + 2").is_ok());
    let _ = quote::quote! { fn generated() {} };
    println!("{}", unsafe { libc::getpid() } > 0);
    println!("{}", CustomError::Bad);
    let _: anyhow::Error = anyhow::anyhow!("checked");
    Ok(())
}
