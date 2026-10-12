//! Board entry adapter for spacemit-rt's entry macro.

use proc_macro::TokenStream;
use proc_macro2::TokenStream as Tokens;
use quote::quote;
use syn::{FnArg, ItemFn, ReturnType, Type, parse_macro_input, spanned::Spanned};

/// Initializes board devices and the console before calling `fn(Board)`.
#[proc_macro_attribute]
pub fn entry(args: TokenStream, input: TokenStream) -> TokenStream {
    if !args.is_empty() {
        return syn::Error::new(proc_macro2::Span::call_site(), "expected #[entry]")
            .to_compile_error()
            .into();
    }
    let input = parse_macro_input!(input as ItemFn);
    let expanded = (|| expand(input, crate_path("satboot")?, crate_path("spacemit-rt")?))();
    expanded
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

fn crate_path(package: &str) -> syn::Result<Tokens> {
    let name = match proc_macro_crate::crate_name(package) {
        Ok(proc_macro_crate::FoundCrate::Itself) => package.replace('-', "_"),
        Ok(proc_macro_crate::FoundCrate::Name(name)) => name,
        Err(error) => {
            return Err(syn::Error::new(proc_macro2::Span::call_site(), error));
        }
    };
    let name = syn::Ident::new(&name, proc_macro2::Span::call_site());
    Ok(quote!(::#name))
}

fn expand(input: ItemFn, bootloader: Tokens, runtime: Tokens) -> syn::Result<Tokens> {
    // Only these signature parts change; the runtime validates the rest.
    let argument = matches!(input.sig.inputs.first(), Some(FnArg::Typed(arg))
        if matches!(arg.ty.as_ref(), Type::Path(_)));
    let output = match &input.sig.output {
        ReturnType::Default => true,
        ReturnType::Type(_, ty) => {
            matches!(ty.as_ref(), Type::Never(_))
                || matches!(ty.as_ref(), Type::Tuple(tuple) if tuple.elems.is_empty())
        }
    };
    if input.sig.inputs.len() != 1 || !argument || !output {
        return Err(syn::Error::new(
            input.span(),
            "expected fn(Board) with return type () or !",
        ));
    }
    let name = &input.sig.ident;
    let peripherals = syn::Ident::new("peripherals", proc_macro2::Span::mixed_site());
    let board = syn::Ident::new("board", proc_macro2::Span::mixed_site());
    let run = syn::Ident::new("__satboot_main", proc_macro2::Span::mixed_site());
    let store = syn::Ident::new("__satboot_store", proc_macro2::Span::mixed_site());
    let mut wrapper = input.clone();
    wrapper.sig.inputs = syn::parse_quote!(#peripherals: #runtime::Peripherals);
    wrapper.sig.output = ReturnType::Default;
    wrapper.block = syn::parse_quote!({
        // Keep board/result storage out of the runtime's entry frame.
        #[inline(never)]
        fn #run(#peripherals: &'static mut #runtime::Peripherals) {
            #[inline(never)]
            #input

            if let ::core::result::Result::Ok(#board) =
                #bootloader::platform::init_board(#peripherals)
            {
                #name(#board);
            }
        }

        #[inline]
        fn #store(#peripherals: #runtime::Peripherals) -> &'static mut #runtime::Peripherals {
            static mut PERIPHERALS: ::core::mem::MaybeUninit<#runtime::Peripherals> =
                ::core::mem::MaybeUninit::uninit();
            // SAFETY: The runtime invokes this wrapper once on the boot hart.
            // Move its unique tokens into permanent storage for console borrows.
            unsafe {
                let slot = ::core::ptr::addr_of_mut!(PERIPHERALS)
                    .cast::<#runtime::Peripherals>();
                slot.write(#peripherals);
                &mut *slot
            }
        }
        let stored = #store(#peripherals);
        #run(stored);
    });
    Ok(quote! {
        #[#runtime::entry]
        #wrapper
    })
}
