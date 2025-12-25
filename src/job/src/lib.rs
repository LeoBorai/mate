use proc_macro::TokenStream;
use quote::quote;
use syn::{ItemFn, parse_macro_input};

#[proc_macro_attribute]
pub fn mate_object(_attr: TokenStream, item: TokenStream) -> TokenStream {
    let input = parse_macro_input!(item as syn::DeriveInput);
    let name = &input.ident;

    let expanded = quote! {
        #[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
        #input

        const _: () = {
            fn assert_impl<T: serde::Serialize + serde::de::DeserializeOwned + Clone + std::fmt::Debug>() {}
            fn assert_type() {
                assert_impl::<#name>();
            }
        };
    };

    TokenStream::from(expanded)
}

#[proc_macro_attribute]
pub fn mate_handler(_attr: TokenStream, item: TokenStream) -> TokenStream {
    let input_fn = parse_macro_input!(item as ItemFn);
    let fn_name = &input_fn.sig.ident;
    let fn_vis = &input_fn.vis;
    let fn_block = &input_fn.block;
    let fn_attrs = &input_fn.attrs;
    let fn_sig = &input_fn.sig;
    let _inputs = &fn_sig.inputs;
    let _output = &fn_sig.output;

    let expanded = quote! {
        #(#fn_attrs)*
        #fn_vis #fn_sig {
            #fn_block
        }

        #[wstd::main]
        async fn main() -> Result<(), Box<dyn std::error::Error>> {
            match #fn_name().await {
                Ok(_) => Ok(()),
                Err(e) => {
                    Err(e.into())
                }
            }
        }
    };

    TokenStream::from(expanded)
}
