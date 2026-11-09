use proc_macro::TokenStream;

#[proc_macro]
pub fn host_value(_input: TokenStream) -> TokenStream {
    format!("{}u32", worker::value()).parse().unwrap()
}
