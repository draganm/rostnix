use proc_macro::{Literal, TokenStream, TokenTree};

#[proc_macro]
pub fn app_name(_input: TokenStream) -> TokenStream {
    TokenTree::Literal(Literal::string("ws-app")).into()
}
