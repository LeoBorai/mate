use proc_macro::TokenStream;
use quote::quote;
use syn::{FnArg, ItemFn, ReturnType, parse_macro_input};

#[proc_macro_attribute]
pub fn mate_handler(_attr: TokenStream, item: TokenStream) -> TokenStream {
    let input_fn = parse_macro_input!(item as ItemFn);
    let fn_name = &input_fn.sig.ident;
    let input_type = match input_fn.sig.inputs.first() {
        Some(FnArg::Typed(pat_type)) => &pat_type.ty,
        _ => {
            return syn::Error::new_spanned(
                &input_fn.sig,
                "Function must have exactly one parameter of type Input",
            )
            .to_compile_error()
            .into();
        }
    };

    let output_type = match &input_fn.sig.output {
        ReturnType::Type(_, ty) => ty.as_ref(),
        ReturnType::Default => {
            return syn::Error::new_spanned(
                &input_fn.sig,
                "Function must return a type that implements Serialize",
            )
            .to_compile_error()
            .into();
        }
    };

    let expanded = quote! {
        #input_fn

        #[unsafe(no_mangle)]
        pub extern "C" fn process(input_ptr: *const u8, input_len: usize) -> *mut u8 {
            let input_slice = unsafe {
                std::slice::from_raw_parts(input_ptr, input_len)
            };

            let input: #input_type = match serde_json::from_slice(input_slice) {
                Ok(input) => input,
                Err(e) => {
                    eprintln!("Failed to deserialize input: {}", e);
                    return std::ptr::null_mut();
                }
            };

            let output: #output_type = #fn_name(input);
            let output_json = match serde_json::to_vec(&output) {
                Ok(json) => json,
                Err(e) => {
                    eprintln!("Failed to serialize output: {}", e);
                    return std::ptr::null_mut();
                }
            };

            let output_len = output_json.len();
            let mut result = vec![0u8; 4 + output_len];
            result[0..4].copy_from_slice(&(output_len as u32).to_le_bytes());
            result[4..].copy_from_slice(&output_json);

            let ptr = result.as_mut_ptr();
            std::mem::forget(result); // Prevent deallocation
            ptr
        }

        #[unsafe(no_mangle)]
        pub extern "C" fn allocate(size: usize) -> *mut u8 {
            let mut buf = Vec::with_capacity(size);
            let ptr = buf.as_mut_ptr();
            std::mem::forget(buf);
            ptr
        }

        #[unsafe(no_mangle)]
        pub extern "C" fn deallocate(ptr: *mut u8, size: usize) {
            unsafe {
                let _ = Vec::from_raw_parts(ptr, size, size);
            }
        }
    };

    TokenStream::from(expanded)
}

#[proc_macro_attribute]
pub fn mate_record(_attr: TokenStream, item: TokenStream) -> TokenStream {
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
