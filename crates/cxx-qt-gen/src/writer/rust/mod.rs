// SPDX-FileCopyrightText: 2022 Klarälvdalens Datakonsult AB, a KDAB Group company <info@kdab.com>
// SPDX-FileContributor: Andrew Hayzen <andrew.hayzen@kdab.com>
//
// SPDX-License-Identifier: MIT OR Apache-2.0

use crate::generator::rust::{qobject::GeneratedRustQObject, GeneratedRustBlocks};
use convert_case::{Case, Casing};
use proc_macro2::TokenStream;
use quote::{format_ident, quote, ToTokens};
use syn::Ident;

/// Mangle an input name with an object name
///
/// For now we need to do this to avoid free Rust methods from colliding
/// Once static methods are possible in CXX this could be removed
/// https://github.com/dtolnay/cxx/issues/447
fn mangle(name: &str, object: &Ident) -> Ident {
    format_ident!(
        "{}",
        format!("{name}_{object}", name = name, object = object).to_case(Case::Snake)
    )
}

/// Return common blocks for CXX bridge which the C++ writer adds as well
fn cxx_bridge_common_blocks(qobject: &GeneratedRustQObject) -> Vec<TokenStream> {
    let cpp_struct_ident = &qobject.cpp_struct_ident;
    let rust_struct_ident = &qobject.rust_struct_ident;
    let cxx_qt_thread_ident = &qobject.cxx_qt_thread_ident;
    let namespace_internals = &qobject.namespace_internals;

    let new_cpp_obj_str = mangle("new_cpp_object", cpp_struct_ident).to_string();
    let create_rs_ident = mangle("create_rs", rust_struct_ident);

    vec![
        quote! {
            unsafe extern "C++" {
                // Specialised version of CxxQtThread
                //
                // CXX doesn't support having generic types in the function yet
                // so we cannot have CxxQtThread<T> in cxx-qt-lib and then use that here
                // For now we use a type alias on C++ then use it like a normal type here
                // https://github.com/dtolnay/cxx/issues/683
                type #cxx_qt_thread_ident;

                #[cxx_name = "unsafeRust"]
                fn rust(self: &#cpp_struct_ident) -> &#rust_struct_ident;

                #[cxx_name = "qtThread"]
                fn qt_thread(self: &#cpp_struct_ident) -> UniquePtr<#cxx_qt_thread_ident>;
                fn queue(self: &#cxx_qt_thread_ident, func: fn(ctx: Pin<&mut #cpp_struct_ident>)) -> Result<()>;

                #[rust_name = #new_cpp_obj_str]
                #[namespace = #namespace_internals]
                fn newCppObject() -> UniquePtr<#cpp_struct_ident>;
            }
        },
        quote! {
            extern "C++" {
                #[cxx_name = "unsafeRustMut"]
                unsafe fn rust_mut(self: Pin<&mut #cpp_struct_ident>) -> Pin<&mut #rust_struct_ident>;
            }
        },
        quote! {
            extern "Rust" {
                #[cxx_name = "createRs"]
                #[namespace = #namespace_internals]
                fn #create_rs_ident() -> Box<#rust_struct_ident>;
            }
        },
    ]
}

/// Return common blocks for CXX-Qt implementation which the C++ writer adds as well
fn cxx_qt_common_blocks(qobject: &GeneratedRustQObject) -> Vec<TokenStream> {
    let rust_struct_ident = &qobject.rust_struct_ident;
    let cxx_qt_thread_ident = &qobject.cxx_qt_thread_ident;
    let create_rs_ident = mangle("create_rs", rust_struct_ident);

    vec![
        quote! {
            unsafe impl Send for #cxx_qt_thread_ident {}
        },
        quote! {
            pub fn #create_rs_ident() -> std::boxed::Box<#rust_struct_ident> {
                std::default::Default::default()
            }
        },
    ]
}

/// For a given GeneratedRustBlocks write this into a Rust TokenStream
pub fn write_rust(generated: &GeneratedRustBlocks) -> TokenStream {
    // Build the module idents
    let cxx_mod_ident = &generated.cxx_mod.ident;
    let cxx_qt_mod_ident = format_ident!("cxx_qt_{}", cxx_mod_ident);

    // Retrieve the module contents and namespace
    let mut cxx_mod = generated.cxx_mod.clone();
    let mut cxx_mod_contents = generated.cxx_mod_contents.clone();
    let mut cxx_qt_mod_contents = generated.cxx_qt_mod_contents.clone();
    let namespace = &generated.namespace;

    // Add common includes for all objects
    cxx_mod_contents.insert(
        0,
        syn::parse2(quote! {
            unsafe extern "C++" {
                include ! (< QtCore / QObject >);
                include!("cxx-qt-lib/include/convert.h");
                include!("cxx-qt-lib/include/cxxqt_thread.h");
            }
        })
        .expect("Could not build CXX common block"),
    );

    for qobject in &generated.qobjects {
        // Add the common blocks into the bridge which we need
        cxx_mod_contents.extend_from_slice(&qobject.blocks.cxx_mod_contents);
        cxx_mod_contents.append(
            &mut cxx_bridge_common_blocks(qobject)
                .into_iter()
                .map(|block| syn::parse2(block).expect("Could not build CXX common block"))
                .collect(),
        );

        // Inject the common blocks into the implementation we need
        cxx_qt_mod_contents.extend_from_slice(&qobject.blocks.cxx_qt_mod_contents);
        cxx_qt_mod_contents.append(
            &mut cxx_qt_common_blocks(qobject)
                .into_iter()
                .map(|block| syn::parse2(block).expect("Could not build CXX-Qt common block"))
                .collect(),
        );
    }

    // Inject the CXX blocks
    if let Some((_, items)) = &mut cxx_mod.content {
        items.extend(cxx_mod_contents.into_iter());
    } else {
        cxx_mod.content = Some((syn::token::Brace::default(), cxx_mod_contents));
    }

    quote! {
        #[cxx::bridge(namespace = #namespace)]
        #cxx_mod

        pub use self::#cxx_qt_mod_ident::*;
        mod #cxx_qt_mod_ident {
            use super::#cxx_mod_ident::*;
            use std::pin::Pin;

            type UniquePtr<T> = cxx::UniquePtr<T>;

            #(#cxx_qt_mod_contents)*
        }
    }
    .into_token_stream()
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::{
        generator::rust::qobject::{GeneratedRustQObject, GeneratedRustQObjectBlocks},
        tests::tokens_to_syn,
    };
    use pretty_assertions::assert_str_eq;
    use quote::format_ident;

    /// Helper to create a GeneratedRustBlocks for testing
    pub fn create_generated_rust() -> GeneratedRustBlocks {
        GeneratedRustBlocks {
            cxx_mod: tokens_to_syn(quote! {
                mod ffi {}
            }),
            cxx_mod_contents: vec![tokens_to_syn(quote! {
                unsafe extern "C++" {
                    include!("myobject.cxxqt.h");
                }
            })],
            cxx_qt_mod_contents: vec![tokens_to_syn(quote! {
                use module::Struct;
            })],
            namespace: "cxx_qt::my_object".to_owned(),
            qobjects: vec![GeneratedRustQObject {
                cpp_struct_ident: format_ident!("MyObjectQt"),
                cxx_qt_thread_ident: format_ident!("MyObjectCxxQtThread"),
                namespace_internals: "cxx_qt::my_object::cxx_qt_my_object".to_owned(),
                rust_struct_ident: format_ident!("MyObject"),
                blocks: GeneratedRustQObjectBlocks {
                    cxx_mod_contents: vec![
                        tokens_to_syn(quote! {
                            unsafe extern "C++" {
                                #[cxx_name = "MyObject"]
                                type MyObjectQt;
                            }
                        }),
                        tokens_to_syn(quote! {
                            extern "Rust" {
                                #[cxx_name = "MyObjectRust"]
                                type MyObject;
                            }
                        }),
                    ],
                    cxx_qt_mod_contents: vec![
                        tokens_to_syn(quote! {
                            #[derive(Default)]
                            pub struct MyObject;
                        }),
                        tokens_to_syn(quote! {
                            impl MyObject {
                                fn rust_method(&self) {

                                }
                            }
                        }),
                    ],
                },
            }],
        }
    }

    /// Helper to create a GeneratedRustBlocks for testing with multiple qobjects
    pub fn create_generated_rust_multi_qobjects() -> GeneratedRustBlocks {
        GeneratedRustBlocks {
            cxx_mod: tokens_to_syn(quote! {
                mod ffi {}
            }),
            cxx_mod_contents: vec![tokens_to_syn(quote! {
                unsafe extern "C++" {
                    include!("multiobject.cxxqt.h");
                }
            })],
            cxx_qt_mod_contents: vec![tokens_to_syn(quote! {
                use module::Struct;
            })],
            namespace: "cxx_qt".to_owned(),
            qobjects: vec![
                GeneratedRustQObject {
                    cpp_struct_ident: format_ident!("FirstObjectQt"),
                    cxx_qt_thread_ident: format_ident!("FirstObjectCxxQtThread"),
                    namespace_internals: "cxx_qt::cxx_qt_first_object".to_owned(),
                    rust_struct_ident: format_ident!("FirstObject"),
                    blocks: GeneratedRustQObjectBlocks {
                        cxx_mod_contents: vec![
                            tokens_to_syn(quote! {
                                unsafe extern "C++" {
                                    #[cxx_name = "FirstObject"]
                                    type FirstObjectQt;
                                }
                            }),
                            tokens_to_syn(quote! {
                                extern "Rust" {
                                    #[cxx_name = "FirstObjectRust"]
                                    type FirstObject;
                                }
                            }),
                        ],
                        cxx_qt_mod_contents: vec![
                            tokens_to_syn(quote! {
                                #[derive(Default)]
                                pub struct FirstObject;
                            }),
                            tokens_to_syn(quote! {
                                impl FirstObject {
                                    fn rust_method(&self) {

                                    }
                                }
                            }),
                        ],
                    },
                },
                GeneratedRustQObject {
                    cpp_struct_ident: format_ident!("SecondObjectQt"),
                    cxx_qt_thread_ident: format_ident!("SecondObjectCxxQtThread"),
                    namespace_internals: "cxx_qt::cxx_qt_second_object".to_owned(),
                    rust_struct_ident: format_ident!("SecondObject"),
                    blocks: GeneratedRustQObjectBlocks {
                        cxx_mod_contents: vec![
                            tokens_to_syn(quote! {
                                unsafe extern "C++" {
                                    #[cxx_name = "SecondObject"]
                                    type SecondObjectQt;
                                }
                            }),
                            tokens_to_syn(quote! {
                                extern "Rust" {
                                    #[cxx_name = "SecondObjectRust"]
                                    type SecondObject;
                                }
                            }),
                        ],
                        cxx_qt_mod_contents: vec![
                            tokens_to_syn(quote! {
                                #[derive(Default)]
                                pub struct SecondObject;
                            }),
                            tokens_to_syn(quote! {
                                impl SecondObject {
                                    fn rust_method(&self) {

                                    }
                                }
                            }),
                        ],
                    },
                },
            ],
        }
    }

    /// Helper for the expected Rust
    pub fn expected_rust() -> String {
        quote! {
            #[cxx::bridge(namespace = "cxx_qt::my_object")]
            mod ffi {
                unsafe extern "C++" {
                    include ! (< QtCore / QObject >);
                    include!("cxx-qt-lib/include/convert.h");
                    include!("cxx-qt-lib/include/cxxqt_thread.h");
                }

                unsafe extern "C++" {
                    include!("myobject.cxxqt.h");
                }

                unsafe extern "C++" {
                    #[cxx_name = "MyObject"]
                    type MyObjectQt;
                }

                extern "Rust" {
                    #[cxx_name = "MyObjectRust"]
                    type MyObject;
                }

                unsafe extern "C++" {
                    type MyObjectCxxQtThread;

                    #[cxx_name = "unsafeRust"]
                    fn rust(self: &MyObjectQt) -> &MyObject;

                    #[cxx_name = "qtThread"]
                    fn qt_thread(self: &MyObjectQt) -> UniquePtr<MyObjectCxxQtThread>;
                    fn queue(self: &MyObjectCxxQtThread, func: fn(ctx: Pin<&mut MyObjectQt>)) -> Result<()>;

                    #[rust_name = "new_cpp_object_my_object_qt"]
                    #[namespace = "cxx_qt::my_object::cxx_qt_my_object"]
                    fn newCppObject() -> UniquePtr<MyObjectQt>;
                }

                extern "C++" {
                    #[cxx_name = "unsafeRustMut"]
                    unsafe fn rust_mut(self: Pin<&mut MyObjectQt>) -> Pin<&mut MyObject>;
                }

                extern "Rust" {
                    #[cxx_name = "createRs"]
                    #[namespace = "cxx_qt::my_object::cxx_qt_my_object"]
                    fn create_rs_my_object() -> Box<MyObject>;
                }
            }

            pub use self::cxx_qt_ffi::*;
            mod cxx_qt_ffi {
                use super::ffi::*;
                use std::pin::Pin;

                type UniquePtr<T> = cxx::UniquePtr<T>;

                use module::Struct;

                #[derive(Default)]
                pub struct MyObject;

                impl MyObject {
                    fn rust_method(&self) {

                    }
                }

                unsafe impl Send for MyObjectCxxQtThread {}

                pub fn create_rs_my_object() -> std::boxed::Box<MyObject> {
                    std::default::Default::default()
                }
            }
        }
        .into_token_stream()
        .to_string()
    }

    /// Helper for the expected Rust with multiple qobjects
    pub fn expected_rust_multi_qobjects() -> String {
        quote! {
            #[cxx::bridge(namespace = "cxx_qt")]
            mod ffi {
                unsafe extern "C++" {
                    include ! (< QtCore / QObject >);
                    include!("cxx-qt-lib/include/convert.h");
                    include!("cxx-qt-lib/include/cxxqt_thread.h");
                }

                unsafe extern "C++" {
                    include!("multiobject.cxxqt.h");
                }

                unsafe extern "C++" {
                    #[cxx_name = "FirstObject"]
                    type FirstObjectQt;
                }

                extern "Rust" {
                    #[cxx_name = "FirstObjectRust"]
                    type FirstObject;
                }

                unsafe extern "C++" {
                    type FirstObjectCxxQtThread;

                    #[cxx_name = "unsafeRust"]
                    fn rust(self: &FirstObjectQt) -> &FirstObject;

                    #[cxx_name = "qtThread"]
                    fn qt_thread(self: &FirstObjectQt) -> UniquePtr<FirstObjectCxxQtThread>;
                    fn queue(self: &FirstObjectCxxQtThread, func: fn(ctx: Pin<&mut FirstObjectQt>)) -> Result<()>;

                    #[rust_name = "new_cpp_object_first_object_qt"]
                    #[namespace = "cxx_qt::cxx_qt_first_object"]
                    fn newCppObject() -> UniquePtr<FirstObjectQt>;
                }

                extern "C++" {
                    #[cxx_name = "unsafeRustMut"]
                    unsafe fn rust_mut(self: Pin<&mut FirstObjectQt>) -> Pin<&mut FirstObject>;
                }

                extern "Rust" {
                    #[cxx_name = "createRs"]
                    #[namespace = "cxx_qt::cxx_qt_first_object"]
                    fn create_rs_first_object() -> Box<FirstObject>;
                }
                unsafe extern "C++" {
                    #[cxx_name = "SecondObject"]
                    type SecondObjectQt;
                }

                extern "Rust" {
                    #[cxx_name = "SecondObjectRust"]
                    type SecondObject;
                }

                unsafe extern "C++" {
                    type SecondObjectCxxQtThread;

                    #[cxx_name = "unsafeRust"]
                    fn rust(self: &SecondObjectQt) -> &SecondObject;

                    #[cxx_name = "qtThread"]
                    fn qt_thread(self: &SecondObjectQt) -> UniquePtr<SecondObjectCxxQtThread>;
                    fn queue(self: &SecondObjectCxxQtThread, func: fn(ctx: Pin<&mut SecondObjectQt>)) -> Result<()>;

                    #[rust_name = "new_cpp_object_second_object_qt"]
                    #[namespace = "cxx_qt::cxx_qt_second_object"]
                    fn newCppObject() -> UniquePtr<SecondObjectQt>;
                }

                extern "C++" {
                    #[cxx_name = "unsafeRustMut"]
                    unsafe fn rust_mut(self: Pin<&mut SecondObjectQt>) -> Pin<&mut SecondObject>;
                }

                extern "Rust" {
                    #[cxx_name = "createRs"]
                    #[namespace = "cxx_qt::cxx_qt_second_object"]
                    fn create_rs_second_object() -> Box<SecondObject>;
                }
            }

            pub use self::cxx_qt_ffi::*;
            mod cxx_qt_ffi {
                use super::ffi::*;
                use std::pin::Pin;

                type UniquePtr<T> = cxx::UniquePtr<T>;

                use module::Struct;

                #[derive(Default)]
                pub struct FirstObject;

                impl FirstObject {
                    fn rust_method(&self) {

                    }
                }

                unsafe impl Send for FirstObjectCxxQtThread {}

                pub fn create_rs_first_object() -> std::boxed::Box<FirstObject> {
                    std::default::Default::default()
                }

                #[derive(Default)]
                pub struct SecondObject;

                impl SecondObject {
                    fn rust_method(&self) {

                    }
                }

                unsafe impl Send for SecondObjectCxxQtThread {}

                pub fn create_rs_second_object() -> std::boxed::Box<SecondObject> {
                    std::default::Default::default()
                }
            }
        }
        .into_token_stream()
        .to_string()
    }

    #[test]
    fn test_write_rust() {
        let generated = create_generated_rust();
        let result = write_rust(&generated);
        assert_str_eq!(result.to_string(), expected_rust());
    }

    #[test]
    fn test_write_rust_multi_qobjects() {
        let generated = create_generated_rust_multi_qobjects();
        let result = write_rust(&generated);
        assert_str_eq!(result.to_string(), expected_rust_multi_qobjects());
    }
}