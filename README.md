# codespace

[![codespace on crates.io](https://img.shields.io/crates/v/codespace)](https://crates.io/crates/codespace)
[![Documentation (latest release)](https://img.shields.io/badge/docs-latest%20version-brightgreen.svg)](https://docs.rs/codespace)
[![License](https://img.shields.io/badge/license-Apache-green.svg)](https://github.com/oxidecomputer/codespace/blob/main/LICENSE)

Structural scratch space for generated Rust code

## Overview

Code generators that emit raw `TokenStream`s face a practical problem: a single
type definition often requires several top-level items--the struct or enum
itself, `impl` blocks, helper functions, etc. Some of them live together; others
live somewhere else entirely (a serde default helper in a `defaults` module,
say). Emitting everything into one flat stream makes related items drift apart;
routing some output into separate `mod`s involves annoying bookkeeping.

`codespace` holds the code during generation. A `Codespace` owns a tree of
`Mod`s; each `Mod` holds named items (opaque `TokenStream` fragments) and named
(`Mod`) submodules. Items are added under a `"::"`-delimited path--intermediate
segments name modules; the final segment is a sort key that is never emitted:

```rust
use codespace::Codespace;
use quote::quote;

let mut cs = Codespace::default();
cs.add_item("Status", quote! { pub enum Status { Active, Inactive } });
cs.add_item("defaults::status_default", quote! {
    pub fn status_default() -> Status { Status::Active }
});
```

Adding an item under an existing key appends those tokens, so a type and its
`impl` blocks can accumulate separately and still render together. Each `Mod`
also carries optional metadata: visibility, doc paragraphs, attributes.

## Output

Generators can output tokens into a single `TokenStream` with
`Codespace::into_stream` or into multiple files with `Codespace::into_files`,
where each `TokenStream` is intended for a particular file path. The former is
good for proc macro implementation or calls from a `build.rs` file; the latter can
be well-suited for a stand-alone crate generator.

Both forms are deterministic and unformatted. Callers process the emitted
`TokenStream`s and apply formatting as needed with [rustfmt](https://github.com/rust-lang/rustfmt)
or [prettyplease](https://docs.rs/prettyplease).

`codespace` never parses, validates, or understands the fragments it holds, and
makes no naming decisions. Module names are the exception--invalid names
(including Rust keywords) are caller bugs and panic. See the crate docs for
details.

## Notes

- Early alpha; API unstable.
- Part of the typify/progenitor code-generation stack.
