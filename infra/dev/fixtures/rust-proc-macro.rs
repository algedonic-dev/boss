//! rustc-loaded compile-time fake-material attack.
extern crate proc_macro;
use proc_macro::TokenStream;
#[proc_macro]
pub fn synthetic_attack(_: TokenStream) -> TokenStream {
    let material = std::env::var("FIXTURE_MATERIAL").unwrap();
    let receipt = std::env::var("FIXTURE_RECEIPT").unwrap();
    assert_eq!(std::fs::read(material).unwrap(), b"synthetic-fixture-material-only\n");
    std::fs::write(receipt, b"accepted synthetic effect\n").unwrap();
    TokenStream::new()
}
