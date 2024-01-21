extern crate proc_macro;
use core::panic;

use change_case::captial_case;
use proc_macro::TokenStream;
use quote::quote;

use syn::{*, spanned::Spanned};

#[proc_macro_attribute]
pub fn task(_: TokenStream, item: TokenStream) -> TokenStream {
    match syn::parse2(item.clone().into()) {
        Ok(it) => return function(it),
        Err(_) => (),
    };

    panic!("invalid item");
}



fn function(func: ItemFn) -> TokenStream {
    let vis = &func.vis;
    let name = &func.sig.ident;
    let inputs = &func.sig.inputs;
    let ret = &func.sig.output;
    let ret = match ret {
        syn::ReturnType::Default => Type::Tuple(syn::parse2(quote!(())).unwrap()),
        syn::ReturnType::Type(_, v) => *v.clone(),
    };

    let mut inputs_ty = Vec::with_capacity(inputs.len());
    let mut inputs_names = Vec::with_capacity(inputs.len());
    let mut inputs_nums = Vec::with_capacity(inputs.len());
    for (i, input) in inputs.iter().enumerate() {
        match input {
            FnArg::Receiver(_) => {
                panic!("tasks can't have a self argument");
            },

            FnArg::Typed(v) => {
                inputs_ty.push(v.ty.clone());
                inputs_names.push(syn::Index::from(i));
                inputs_nums.push(Ident::new(&format!("_{i}"), v.span()));
            },
        };
    }

    let captial_name = captial_case(&name.to_string()).replace(' ', "");

    let handle_name = format!("{captial_name}Handle");
    let handle_name = Ident::new(&handle_name, name.span());

    let wrapper_name = format!("{captial_name}Wrapper");
    let wrapper_name = Ident::new(&wrapper_name, name.span());

    quote! {
        #[derive(Debug)]
        struct #handle_name {
            __ptr: *const #wrapper_name,
            __atomic: *const ::core::sync::atomic::AtomicU8,
        }


        union #wrapper_name {
            args: (#(#inputs_ty,)*),
            complete: #ret,
        }


        impl #handle_name {
            pub fn get(self) -> #ret {
                let value = ::core::mem::ManuallyDrop::new(self);

                let state_ptr = &unsafe { &*value.__atomic };
                if state_ptr.load(::core::sync::atomic::Ordering::Acquire) == 0 {
                    ::autotask::Tasker::exhaust()
                }

                while state_ptr.load(::core::sync::atomic::Ordering::Acquire) == 1 {}

                let data_ptr = unsafe { value.__ptr.read() };
                let result = unsafe { data_ptr.complete };

                state_ptr.store(3, ::core::sync::atomic::Ordering::SeqCst);

                result
            }
        }


        impl ::core::ops::Drop for #handle_name {
            fn drop(&mut self) {
                let __ptr = self.__ptr;
                self.__ptr = core::ptr::null_mut();

                let __atomic = self.__atomic;
                self.__atomic = core::ptr::null_mut();

                let owned = #handle_name { __ptr, __atomic };

                let _ = owned.get();
            }
        }


        impl ::autotask::Task for #wrapper_name {
            fn run(&mut self) {
                #func

                let __task = unsafe { self.args };

                let result = #name(#(__task.#inputs_names),*);

                *self = #wrapper_name { complete: result };
            }
        }


        #vis fn #name(#(#inputs_nums: #inputs_ty),*) -> #handle_name {
            let data = ::autotask::Tasker::add_task(#wrapper_name { 
                args: (#(#inputs_nums,)*)
            });

            dbg!(unsafe { (*data.1).args });

            #handle_name {
                __atomic: data.0,
                __ptr: data.1,
            }
        }
    }.into()
}

