use proc_macro::TokenStream;

#[proc_macro]
pub fn hello(_input: TokenStream) -> TokenStream {
    "9".parse().unwrap()
}

#[proc_macro_attribute]
pub fn tag(_attribute: TokenStream, item: TokenStream) -> TokenStream {
    item
}
