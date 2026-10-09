use proc_macro::{Literal, TokenStream, TokenTree};

fn name() -> &'static str {
    "ws-app"
}

#[proc_macro]
pub fn app_name(_input: TokenStream) -> TokenStream {
    TokenTree::Literal(Literal::string(name())).into()
}

#[cfg(test)]
mod tests {
    #[test]
    fn name_is_the_binary() {
        assert_eq!(super::name(), "ws-app");
    }
}
