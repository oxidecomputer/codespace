// Copyright 2026 Oxide Computer Company

//! A structured container for generated Rust code.
//!
//! Code generators that emit raw [`TokenStream`]s face a practical problem:
//! a single type definition often requires multiple top-level items--the
//! struct or enum itself, helper functions, `impl` blocks, and so on. Putting
//! all of those into one `TokenStream` makes it hard to keep related items
//! together or to route ancillary items (e.g. serde default helpers) into a
//! dedicated module.
//!
//! `codespace` provides a scratch space for accumulating and emitting Rust
//! code. A [`Codespace`] holds a root [`Mod`]; each [`Mod`] holds named items
//! (raw [`TokenStream`] fragments) and named sub-[`Mod`]s in separate maps.
//! When code generation is complete, [`Codespace::into_stream`] flattens
//! everything into a single [`TokenStream`] suitable for writing to a file or
//! handing to a proc-macro output; [`Codespace::into_files`] produces output
//! that can be emitted into multiple files (i.e. one `mod` per file).
//!
//! ## Paths
//!
//! [`Codespace::add_item`] and [`Mod::add_item`] accept a `"::"` delimited
//! path. Each segment except the last names a submodule (and must be a valid
//! Rust identifier). The final segment is an arbitrary sort key that is never
//! emitted as a token; usually this should match the name of the relevant
//! item.
//!
//! ```rust
//! use codespace::Codespace;
//! use quote::quote;
//!
//! let mut cs = Codespace::default();
//! cs.add_item("Status", quote! { pub enum Status { Active, Inactive } });
//! cs.add_item("defaults::status_default", quote! {
//!     pub fn status_default() -> Status { Status::Active }
//! });
//! // Renders to:
//! //   pub enum Status { ... }
//! //   pub mod defaults { pub fn status_default() ... }
//! let tokens: proc_macro2::TokenStream = cs.into_stream();
//! # let _ = tokens;
//! ```
//!
//! ## Ordering
//!
//! Within each [`Mod`], items are emitted in sort-key order and submodules
//! in alphabetical order by name.

use std::{
    collections::{btree_map::Entry, BTreeMap},
    path::{Path, PathBuf},
};

use proc_macro2::{TokenStream, TokenTree};
use quote::{format_ident, quote};

/// Validate a module name segment, panicking with context on failure.
///
/// `name` is the segment being validated; `full` is the complete path or
/// name the caller passed, included in the panic message so that generator
/// bugs are easy to locate. Validity is delegated to [`syn::Ident`] parsing,
/// which rejects Rust keywords in addition to lexically invalid identifiers
/// (keywords are lexically valid, but produce uncompilable module
/// declarations, e.g. `pub mod type {}`). Raw identifiers (`r#` prefix) are
/// not supported and are rejected as invalid.
fn validate_mod_name(name: &str, full: &str) {
    // syn must accept `gen` (an identifier before edition 2024), but our
    // generated code may land in a 2024 crate, so we reject it ourselves.
    if name == "gen" {
        panic!("module name {:?} (in {:?}) is a Rust keyword", name, full);
    }
    if !name.starts_with("r#") && syn::parse_str::<syn::Ident>(name).is_ok() {
        return;
    }
    // Distinguish keywords from lexically invalid names for the panic
    // message: a keyword lexes as a lone identifier even though syn's
    // `Ident` parser rejects it.
    let is_keyword = !name.starts_with("r#")
        && name.parse::<TokenStream>().is_ok_and(|ts| {
            let mut trees = ts.into_iter();
            matches!(
                (trees.next(), trees.next()),
                (Some(TokenTree::Ident(ident)), None) if ident == name
            )
        });
    if is_keyword {
        panic!("module name {:?} (in {:?}) is a Rust keyword", name, full);
    }
    panic!(
        "module name {:?} (in {:?}) is not a valid Rust identifier \
         (raw identifiers are not supported)",
        name, full,
    );
}

/// Visibility of a [`Mod`], as rendered on its `mod` block or declaration.
///
/// The default is [`Visibility::Pub`]. The root [`Mod`]'s visibility is
/// never rendered--the root has no `mod` block or declaration.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Visibility {
    /// Rendered as `pub mod name`.
    #[default]
    Pub,
    /// Rendered as `pub(crate) mod name`.
    Crate,
    /// Rendered as `mod name`.
    Private,
}

impl Visibility {
    /// Tokens rendered before the `mod` keyword; empty for `Private`.
    fn to_tokens(self) -> TokenStream {
        match self {
            Visibility::Pub => quote! { pub },
            Visibility::Crate => quote! { pub(crate) },
            Visibility::Private => TokenStream::new(),
        }
    }
}

/// A structured collection of generated Rust items, organized into a tree of
/// modules.
///
/// `Codespace` is the entry point. It owns a root [`Mod`] whose contents are
/// emitted flat (with no surrounding `mod` block) by
/// [`into_stream`](Codespace::into_stream) or into separate files by
/// [`into_files`](Codespace::into_files).
#[derive(Debug, Default)]
pub struct Codespace {
    root: Mod,
}

impl Codespace {
    /// Add (or extend) an item at the given path.
    ///
    /// `path` is a `"::"` delimited string. All segments except the last must
    /// be valid Rust identifiers (they become `pub mod` names). The final
    /// segment is an arbitrary sort key that is never emitted as a token.
    ///
    /// See [`Mod::add_item`] for the single-level form.
    ///
    /// The tokens must be valid at item position (structs, fns, impls,
    /// consts, etc.); codespace never parses or validates them.
    ///
    /// # Panics
    ///
    /// Panics if any intermediate path segment is not a valid Rust identifier.
    /// Segments that are Rust keywords (e.g. `type`) also panic. Raw
    /// identifiers (`r#` prefix) are not supported and are rejected as
    /// invalid.
    pub fn add_item(&mut self, path: impl Into<String>, tokens: TokenStream) {
        let path = path.into();
        let mut segs = path.split("::").peekable();
        let mut m = &mut self.root;

        loop {
            let seg = segs.next().expect("path must not be empty");
            if segs.peek().is_none() {
                // Last segment is the sort key, not a mod name.
                m.add_item(seg, tokens);
                return;
            }
            // Validate here (rather than via get_mod) so the panic message
            // includes the full path the caller passed.
            validate_mod_name(seg, &path);
            m = m.mods.entry(seg.to_string()).or_default();
        }
    }

    /// Add a named submodule to the root [`Mod`], merging if one already
    /// exists. Convenience for [`Mod::add_mod`] on the root; see that method
    /// for the merge semantics, the re-parenting caveat, and panics.
    pub fn add_mod(&mut self, name: impl Into<String>, m: Mod) {
        self.root.add_mod(name, m);
    }

    /// Return a mutable reference to the root [`Mod`].
    pub fn get_root_mod(&mut self) -> &mut Mod {
        &mut self.root
    }

    /// Convert into the root [`Mod`], consuming the `Codespace`. Useful when
    /// inserting a `Codespace` into another `Codespace`.
    pub fn into_root_mod(self) -> Mod {
        self.root
    }

    /// Consume the [`Codespace`] and render it into a [`TokenStream`].
    ///
    /// The root module's contents are emitted flat. Each submodule is wrapped
    /// in a `pub mod name { ... }` block. Within each level, items are
    /// emitted in sort-key order and submodules alphabetically.
    /// The root module's docs and attributes, if any, are emitted at the very
    /// top of the stream in inner form (`#![doc = "..."]`, `#![...]`). The
    /// root module's visibility is irrelevant here--the root is emitted flat,
    /// with no surrounding `mod` block. Each submodule's docs, attributes,
    /// and [`Visibility`] are emitted on its `mod` block.
    pub fn into_stream(self) -> TokenStream {
        let mut out = self.root.inner_meta();
        out.extend(self.root.into_stream());
        out
    }

    /// Consume the [`Codespace`] and render it into a map of file names to the
    /// intended contents. The root module's name will be `lib.rs`.
    ///
    /// File paths are relative and the layout is adaptive: a submodule with
    /// no child submodules (a leaf) renders as `foo.rs` in its parent's
    /// directory; a submodule with child submodules renders as `foo/mod.rs`,
    /// with its children below `foo/`. There is never a `foo.rs` sibling of
    /// a `foo/` directory, and never a directory for a leaf module.
    ///
    /// For each child, the parent's stream contains a module declaration
    /// carrying the child's [`Visibility`]: `pub mod foo;`,
    /// `pub(crate) mod foo;`, or `mod foo;`. The child's docs and attributes
    /// are emitted at the declaration site, in outer form, immediately
    /// before the declaration--not as inner attributes at the top of the
    /// child's file. The root module's docs and attributes are emitted at
    /// the top of `lib.rs` in inner form (`#![doc = "..."]`, `#![...]`).
    ///
    /// An empty `Codespace` produces a single, empty `lib.rs` entry. No I/O
    /// is performed and the output is unformatted; callers write the files
    /// and run [rustfmt] or [prettyplease] as desired.
    ///
    /// [rustfmt]: https://github.com/rust-lang/rustfmt
    /// [prettyplease]: https://docs.rs/prettyplease
    pub fn into_files(self) -> BTreeMap<PathBuf, TokenStream> {
        let mut files = BTreeMap::new();
        let mut contents = self.root.inner_meta();
        contents.extend(self.root.into_file_contents(Path::new(""), &mut files));
        files.insert(PathBuf::from("lib.rs"), contents);
        files
    }
}

/// A node in the [`Codespace`] tree.
///
/// A `Mod` holds two independent ordered maps:
///
/// - **items**: [`TokenStream`] fragments keyed by an arbitrary sort string
///   (the key is never emitted as a token), emitted in key order.
/// - **submodules**: named nested [`Mod`]s keyed by valid Rust identifiers,
///   rendered as `pub mod name { ... }` blocks in alphabetical order.
///
/// A `Mod` also carries optional metadata--a [`Visibility`], doc text, and
/// attributes--that affects how the module is rendered but not its contents.
/// See [`Mod::set_visibility`], [`Mod::add_docs`], and [`Mod::add_attr`].
#[derive(Debug, Default)]
pub struct Mod {
    items: BTreeMap<String, TokenStream>,
    mods: BTreeMap<String, Mod>,
    vis: Visibility,
    docs: Vec<String>,
    attrs: Vec<TokenStream>,
}

impl Mod {
    /// Add code in this module under the given sort `key`.
    ///
    /// `key` is an arbitrary string used only for ordering--it is never
    /// emitted as a token and does not need to be a valid Rust identifier.
    ///
    /// If an item already exists under `key`, the [`TokenStream`] is appended
    /// to it, allowing multiple fragments to accumulate under a single key:
    ///
    /// ```rust
    /// use codespace::Codespace;
    /// use quote::quote;
    ///
    /// let mut cs = Codespace::default();
    /// cs.get_root_mod().add_item("Foo", quote! { pub struct Foo(u32); });
    /// cs.get_root_mod().add_item(
    ///     "Foo",
    ///     quote! { impl Foo { pub fn value(&self) -> u32 { self.0 } } }
    /// );
    /// // Both fragments appear in the output under the same key.
    /// ```
    ///
    /// The tokens must be valid at item position (structs, fns, impls,
    /// consts, etc.); codespace never parses or validates them.
    pub fn add_item(&mut self, key: impl Into<String>, tokens: TokenStream) {
        self.items
            .entry(key.into())
            .and_modify(|existing| existing.extend(tokens.clone()))
            .or_insert(tokens);
    }

    /// Get (or create) a named submodule.
    ///
    /// If a submodule named `name` already exists it is returned; otherwise a
    /// new empty [`Mod`] is created and returned. Safe to call multiple times
    /// with the same name.
    ///
    /// # Panics
    ///
    /// Panics if `name` is not a valid Rust identifier. Rust keywords (e.g.
    /// `type`) also panic. Raw identifiers (`r#` prefix) are not supported
    /// and are rejected as invalid.
    pub fn get_mod(&mut self, name: impl Into<String>) -> &mut Mod {
        let name = name.into();
        // Validate now so the error points here rather than at render time.
        validate_mod_name(&name, &name);
        self.mods.entry(name).or_default()
    }

    /// Replace a named submodule, returning the previous value if any.
    ///
    /// # Panics
    ///
    /// Panics if `name` is not a valid Rust identifier. Rust keywords (e.g.
    /// `type`) also panic. Raw identifiers (`r#` prefix) are not supported
    /// and are rejected as invalid.
    pub fn replace_mod(&mut self, name: impl Into<String>, m: Mod) -> Option<Mod> {
        let name = name.into();
        // Validate now so the error points here rather than at render time.
        validate_mod_name(&name, &name);
        self.mods.insert(name, m)
    }

    /// Merge another [`Mod`] into this one.
    ///
    /// `other`'s items are appended under their keys (the same semantics as
    /// repeated [`Mod::add_item`] calls), and same-named submodules are
    /// merged recursively; other submodules are inserted. Metadata is merged
    /// as follows: `other`'s doc paragraphs are appended after `self`'s
    /// (the same semantics as repeated [`Mod::add_docs`] calls); `other`'s
    /// attributes are concatenated after `self`'s; if the visibilities
    /// differ, `self`'s is kept.
    pub fn merge(&mut self, other: Mod) {
        for (key, tokens) in other.items {
            self.add_item(key, tokens);
        }
        for (name, m) in other.mods {
            // Names were validated when they were inserted into `other`.
            match self.mods.entry(name) {
                Entry::Occupied(e) => e.into_mut().merge(m),
                Entry::Vacant(e) => {
                    e.insert(m);
                }
            }
        }
        self.docs.extend(other.docs);
        self.attrs.extend(other.attrs);
        // Visibility: self's wins; other.vis is dropped.
    }

    /// Add a named submodule, merging if one already exists.
    ///
    /// If a submodule named `name` already exists, `m` is **merged** into it
    /// via [`Mod::merge`]--contrast with [`Mod::replace_mod`], which clobbers
    /// any existing submodule. Otherwise `m` is inserted as a new submodule.
    ///
    /// Note that re-parenting moves a subtree deeper: relative paths inside
    /// `m`'s token fragments (`super::...`) and `use super::...` lines written
    /// for `m`'s old location will point one level off after mounting.
    /// Fragments intended for mounting must be generated with the final module
    /// shape in mind; codespace cannot detect or fix this (tokens are opaque).
    ///
    /// # Panics
    ///
    /// Panics if `name` is not a valid Rust identifier. Rust keywords (e.g.
    /// `type`) also panic. Raw identifiers (`r#` prefix) are not supported
    /// and are rejected as invalid.
    pub fn add_mod(&mut self, name: impl Into<String>, m: Mod) {
        let name = name.into();
        // Validate now so the error points here rather than at render time.
        validate_mod_name(&name, &name);
        match self.mods.entry(name) {
            Entry::Occupied(e) => e.into_mut().merge(m),
            Entry::Vacant(e) => {
                e.insert(m);
            }
        }
    }

    /// Set this module's [`Visibility`], controlling how its `mod` block or
    /// declaration is rendered. The default is [`Visibility::Pub`]. The root
    /// module's visibility is never rendered--the root has no `mod` block or
    /// declaration.
    ///
    /// Note that trait impls inside a non-`pub` module still apply
    /// program-wide--Rust impls aren't gated by module visibility--so
    /// layouts that route manual impls into a private submodule are viable.
    pub fn set_visibility(&mut self, vis: Visibility) {
        self.vis = vis;
    }

    /// Add a paragraph of doc text to this module.
    ///
    /// `docs` is plain text and may span multiple lines; each line is
    /// rendered as its own doc attribute. Docs accumulate in the order they
    /// are added (as with [`Mod::add_attr`]): each call's text renders as a
    /// separate markdown paragraph, with a blank doc line between calls. On
    /// a submodule, docs are rendered in outer form (`#[doc = "..."]`)
    /// immediately before the `mod`; on the root module, they are rendered
    /// in inner form (`#![doc = "..."]`) at the top of the output.
    pub fn add_docs(&mut self, docs: impl Into<String>) {
        self.docs.push(docs.into());
    }

    /// Add an attribute to this module.
    ///
    /// `attr` is the attribute content only, with no `#[..]` wrapper--e.g.
    /// the tokens `allow(dead_code)`. On a submodule, attributes are rendered
    /// in outer form (`#[...]`) immediately before the `mod`; on the root
    /// module, they are rendered in inner form (`#![...]`) at the top of the
    /// output. Attributes accumulate in the order they are added.
    pub fn add_attr(&mut self, attr: TokenStream) {
        self.attrs.push(attr);
    }

    /// Render this module's docs and attributes in outer form
    /// (`#[doc = "..."]`, `#[...]`) for emission immediately before its
    /// `mod` block or declaration.
    fn outer_meta(&self) -> TokenStream {
        let mut out = TokenStream::new();
        for (ii, block) in self.docs.iter().enumerate() {
            // A blank doc line between blocks keeps each add_docs call a
            // separate markdown paragraph.
            if ii > 0 {
                out.extend(quote! { #[doc = ""] });
            }
            for line in block.lines() {
                out.extend(quote! { #[doc = #line] });
            }
        }
        for attr in &self.attrs {
            out.extend(quote! { #[#attr] });
        }
        out
    }

    /// Render this module's docs and attributes in inner form
    /// (`#![doc = "..."]`, `#![...]`) for emission at the top of a stream or
    /// file. Used only for the root module.
    fn inner_meta(&self) -> TokenStream {
        let mut out = TokenStream::new();
        for (ii, block) in self.docs.iter().enumerate() {
            // A blank doc line between blocks keeps each add_docs call a
            // separate markdown paragraph.
            if ii > 0 {
                out.extend(quote! { #![doc = ""] });
            }
            for line in block.lines() {
                out.extend(quote! { #![doc = #line] });
            }
        }
        for attr in &self.attrs {
            out.extend(quote! { #![#attr] });
        }
        out
    }

    /// Consume this module and render its contents into a [`TokenStream`].
    ///
    /// Items are emitted first in sort-key order, followed by submodules in
    /// alphabetical order. Each submodule is wrapped in `pub mod name { ... }`.
    /// The output has no surrounding `mod` block. Each submodule's docs and
    /// attributes are emitted in outer form immediately before its `mod`
    /// block, and its [`Visibility`] determines the tokens before `mod`
    /// (`pub` by default). This module's own metadata is not emitted here;
    /// that is the caller's responsibility.
    fn into_stream(self) -> TokenStream {
        let mut out = TokenStream::new();
        for (_, tokens) in self.items {
            out.extend(tokens);
        }
        for (name, m) in self.mods {
            let ident = format_ident!("{}", name);
            let meta = m.outer_meta();
            let vis = m.vis.to_tokens();
            let contents = m.into_stream();
            out.extend(quote! {
                #meta
                #vis mod #ident {
                    #contents
                }
            });
        }
        out
    }

    /// Render this module's contents for file mode: a `mod` declaration for
    /// each submodule first (alphabetically), carrying the submodule's
    /// visibility, docs, and attributes--per Rust convention, declarations
    /// sit at the top of a file--then items (in sort-key order). Each
    /// submodule's own file is inserted into `files`: `name.rs` under `dir`
    /// for a leaf (no child submodules), or `name/mod.rs` for a module with
    /// children, which then recurse below `dir/name/`. As with
    /// [`Mod::into_stream`], this module's own metadata is not emitted here.
    fn into_file_contents(
        self,
        dir: &Path,
        files: &mut BTreeMap<PathBuf, TokenStream>,
    ) -> TokenStream {
        let mut out = TokenStream::new();
        for (name, m) in self.mods {
            let ident = format_ident!("{}", name);
            let meta = m.outer_meta();
            let vis = m.vis.to_tokens();
            out.extend(quote! {
                #meta
                #vis mod #ident;
            });
            let (file, child_dir) = if m.mods.is_empty() {
                (dir.join(format!("{}.rs", name)), dir.to_path_buf())
            } else {
                let child_dir = dir.join(&name);
                (child_dir.join("mod.rs"), child_dir)
            };
            let contents = m.into_file_contents(&child_dir, files);
            files.insert(file, contents);
        }
        for (_, tokens) in self.items {
            out.extend(tokens);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use quote::quote;

    #[test]
    fn empty_codespace() {
        let cs = Codespace::default();
        assert!(cs.into_stream().is_empty());
    }

    #[test]
    fn root_items_are_flat() {
        let mut cs = Codespace::default();
        cs.add_item("Foo", quote! { pub struct Foo; });
        cs.add_item("Bar", quote! { pub struct Bar; });
        let out = cs.into_stream().to_string();
        assert!(out.contains("struct Bar"));
        assert!(out.contains("struct Foo"));
        let bar_pos = out.find("struct Bar").unwrap();
        let foo_pos = out.find("struct Foo").unwrap();
        assert!(bar_pos < foo_pos, "expected alphabetical order");
    }

    #[test]
    fn extend_same_key() {
        let mut cs = Codespace::default();
        cs.add_item("Foo", quote! { pub struct Foo; });
        cs.add_item("Foo", quote! { impl Foo {} });
        let out = cs.into_stream().to_string();
        assert!(out.contains("struct Foo"));
        assert!(out.contains("impl Foo"));
    }

    #[test]
    fn path_creates_submod() {
        let mut cs = Codespace::default();
        cs.add_item("Foo", quote! { pub struct Foo; });
        cs.add_item("defaults::foo_value", quote! { pub fn foo_value() {} });
        let out = cs.into_stream().to_string();
        assert!(out.contains("pub mod defaults"));
        assert!(out.contains("fn foo_value"));
        let item_pos = out.find("struct Foo").unwrap();
        let mod_pos = out.find("pub mod defaults").unwrap();
        assert!(item_pos < mod_pos, "items precede submodules");
    }

    #[test]
    fn path_nested() {
        let mut cs = Codespace::default();
        cs.add_item("outer::inner::deep", quote! { pub fn deep() {} });
        let out = cs.into_stream().to_string();
        assert!(out.contains("pub mod outer"));
        assert!(out.contains("pub mod inner"));
        assert!(out.contains("fn deep"));
    }

    #[test]
    fn items_before_mods() {
        let mut cs = Codespace::default();
        cs.add_item("zzz", quote! { pub struct Zzz; });
        cs.add_item("aaa::f", quote! { pub fn f() {} });
        let out = cs.into_stream().to_string();
        let item_pos = out.find("struct Zzz").unwrap();
        let mod_pos = out.find("pub mod aaa").unwrap();
        assert!(item_pos < mod_pos, "items always precede submodules");
    }

    #[test]
    fn get_root_mod_direct_access() {
        let mut cs = Codespace::default();
        cs.get_root_mod()
            .get_mod("defaults")
            .add_item("f", quote! { pub fn f() {} });
        let out = cs.into_stream().to_string();
        assert!(out.contains("pub mod defaults"));
        assert!(out.contains("fn f"));
    }

    #[test]
    fn same_path_twice_extends() {
        let mut cs = Codespace::default();
        cs.add_item("defaults::a", quote! { pub fn a() {} });
        cs.add_item("defaults::b", quote! { pub fn b() {} });
        let out = cs.into_stream().to_string();
        let mod_start = out.find("pub mod defaults").unwrap();
        assert!(out[mod_start..].contains("fn a"));
        assert!(out[mod_start..].contains("fn b"));
    }

    #[test]
    fn item_and_mod_may_share_name() {
        let mut cs = Codespace::default();
        cs.add_item("foo", quote! { pub struct Foo; });
        cs.add_item("foo::bar", quote! { pub fn bar() {} });
        let out = cs.into_stream().to_string();
        assert!(out.contains("struct Foo"));
        assert!(out.contains("pub mod foo"));
        assert!(out.contains("fn bar"));
    }

    #[test]
    #[should_panic]
    fn invalid_mod_name_panics() {
        let mut cs = Codespace::default();
        cs.add_item("not-valid-ident::key", quote! {});
    }

    #[test]
    fn mod_add_item_colons_are_literal_sort_key() {
        // Mod::add_item does NOT do path splitting - "::" is just a sort key char.
        let mut cs = Codespace::default();
        cs.get_root_mod().add_item("a::b", quote! { pub struct X; });
        let out = cs.into_stream().to_string();
        // No submodule should exist; the item appears flat at root.
        assert!(!out.contains("pub mod"), "no submodule expected");
        assert!(out.contains("struct X"));
    }

    #[test]
    fn special_sort_key_orders_before_alpha() {
        // '#' (ASCII 35) sorts before all lowercase letters, so "#preamble" items
        // appear before e.g. "Foo".
        let mut cs = Codespace::default();
        cs.get_root_mod()
            .add_item("Foo", quote! { pub struct Foo; });
        cs.get_root_mod()
            .add_item("#preamble", quote! { use std::collections::BTreeMap; });
        let out = cs.into_stream().to_string();
        let use_pos = out.find("BTreeMap").unwrap();
        let foo_pos = out.find("struct Foo").unwrap();
        assert!(use_pos < foo_pos, "#preamble should sort before Foo");
    }

    #[test]
    fn multiple_submods_alphabetical() {
        let mut cs = Codespace::default();
        cs.add_item("zzz::a", quote! { pub fn a() {} });
        cs.add_item("aaa::b", quote! { pub fn b() {} });
        cs.add_item("mmm::c", quote! { pub fn c() {} });
        let out = cs.into_stream().to_string();
        let aaa = out.find("mod aaa").unwrap();
        let mmm = out.find("mod mmm").unwrap();
        let zzz = out.find("mod zzz").unwrap();
        assert!(
            aaa < mmm && mmm < zzz,
            "submods should appear alphabetically"
        );
    }

    #[test]
    #[should_panic]
    fn get_mod_invalid_ident_panics() {
        let mut cs = Codespace::default();
        cs.get_root_mod().get_mod("not-valid");
    }

    #[test]
    #[should_panic(expected = "is a Rust keyword")]
    fn keyword_mod_name_panics() {
        let mut cs = Codespace::default();
        cs.add_item("type::key", quote! {});
    }

    #[test]
    #[should_panic(expected = "is a Rust keyword")]
    fn get_mod_keyword_panics() {
        let mut cs = Codespace::default();
        cs.get_root_mod().get_mod("type");
    }

    #[test]
    #[should_panic(expected = "is a Rust keyword")]
    fn gen_mod_name_panics() {
        // Reserved only in edition 2024; syn accepts it, we don't.
        let mut cs = Codespace::default();
        cs.get_root_mod().get_mod("gen");
    }

    #[test]
    #[should_panic(expected = "raw identifiers are not supported")]
    fn raw_ident_mod_name_panics() {
        let mut cs = Codespace::default();
        cs.get_root_mod().get_mod("r#type");
    }

    /// Strip all whitespace so tests are robust to token-spacing details.
    fn no_ws(s: &str) -> String {
        s.chars().filter(|c| !c.is_whitespace()).collect()
    }

    #[test]
    fn visibility_pub_renders() {
        let mut cs = Codespace::default();
        cs.get_root_mod()
            .get_mod("foo")
            .add_item("f", quote! { pub fn f() {} });
        let out = no_ws(&cs.into_stream().to_string());
        assert!(out.contains("pubmodfoo{"));
    }

    #[test]
    fn visibility_crate_renders() {
        let mut cs = Codespace::default();
        let m = cs.get_root_mod().get_mod("foo");
        m.set_visibility(Visibility::Crate);
        m.add_item("f", quote! { pub fn f() {} });
        let out = no_ws(&cs.into_stream().to_string());
        assert!(out.contains("pub(crate)modfoo{"));
    }

    #[test]
    fn visibility_private_renders() {
        let mut cs = Codespace::default();
        let m = cs.get_root_mod().get_mod("foo");
        m.set_visibility(Visibility::Private);
        m.add_item("f", quote! { pub fn f() {} });
        let out = no_ws(&cs.into_stream().to_string());
        assert!(out.contains("modfoo{"));
        assert!(!out.contains("pubmodfoo"));
        assert!(!out.contains("pub(crate)"));
    }

    #[test]
    fn submodule_docs_and_attrs_are_outer() {
        let mut cs = Codespace::default();
        let m = cs.get_root_mod().get_mod("helpers");
        m.add_docs("Helper functions.");
        m.add_attr(quote! { allow(dead_code) });
        m.add_item("f", quote! { pub fn f() {} });
        let out = no_ws(&cs.into_stream().to_string());
        assert!(out.contains(r##"#[doc="Helperfunctions."]"##));
        // Metadata appears immediately before the mod, in outer form.
        assert!(out.contains(r##"#[allow(dead_code)]pubmodhelpers{"##));
        assert!(!out.contains("#!"));
    }

    #[test]
    fn merge_appends_items_on_key_collision() {
        let mut a = Mod::default();
        a.add_item("Foo", quote! { pub struct Foo; });
        let mut b = Mod::default();
        b.add_item("Foo", quote! { impl Foo {} });
        b.add_item("Bar", quote! { pub struct Bar; });
        a.merge(b);
        let mut cs = Codespace::default();
        *cs.get_root_mod() = a;
        let out = cs.into_stream().to_string();
        assert!(out.contains("struct Foo"));
        assert!(out.contains("impl Foo"));
        assert!(out.contains("struct Bar"));
        // Both "Foo" fragments accumulated under one key, so they render
        // adjacent to each other and after "Bar".
        let bar_pos = out.find("struct Bar").unwrap();
        let foo_pos = out.find("struct Foo").unwrap();
        assert!(bar_pos < foo_pos);
    }

    #[test]
    fn merge_recursive_submodules() {
        let mut a = Mod::default();
        a.get_mod("shared").add_item("a", quote! { pub fn a() {} });
        let mut b = Mod::default();
        b.get_mod("shared").add_item("b", quote! { pub fn b() {} });
        b.get_mod("only_b").add_item("c", quote! { pub fn c() {} });
        a.merge(b);
        let mut cs = Codespace::default();
        *cs.get_root_mod() = a;
        let out = cs.into_stream().to_string();
        // "shared" appears once, containing both fns. Submodules render
        // alphabetically, so "only_b" precedes "shared".
        assert_eq!(out.matches("mod shared").count(), 1);
        let only_b = out.find("mod only_b").unwrap();
        let shared = out.find("mod shared").unwrap();
        assert!(only_b < shared);
        assert!(out[only_b..shared].contains("fn c"));
        assert!(out[shared..].contains("fn a"));
        assert!(out[shared..].contains("fn b"));
    }

    #[test]
    fn merge_docs_attrs_and_visibility() {
        let mut a = Mod::default();
        a.add_docs("First.");
        a.add_attr(quote! { allow(dead_code) });
        a.set_visibility(Visibility::Crate);
        let mut b = Mod::default();
        b.add_docs("Second.");
        b.add_attr(quote! { allow(unused) });
        b.set_visibility(Visibility::Private);
        a.merge(b);
        // Doc paragraphs concatenated; attrs concatenated; self's vis kept.
        let mut cs = Codespace::default();
        cs.get_root_mod().add_mod("m", a);
        let out = no_ws(&cs.into_stream().to_string());
        assert!(out.contains(r##"#[doc="First."]#[doc=""]#[doc="Second."]"##));
        assert!(out.contains("#[allow(dead_code)]#[allow(unused)]"));
        assert!(out.contains("pub(crate)modm"));
    }

    #[test]
    fn add_mod_inserts_new() {
        let mut cs = Codespace::default();
        let mut m = Mod::default();
        m.add_item("f", quote! { pub fn f() {} });
        cs.add_mod("client", m);
        let out = cs.into_stream().to_string();
        assert!(out.contains("pub mod client"));
        assert!(out.contains("fn f"));
    }

    #[test]
    fn add_mod_merges_on_collision() {
        let mut cs = Codespace::default();
        cs.add_item("client::a", quote! { pub fn a() {} });
        let mut m = Mod::default();
        m.add_item("b", quote! { pub fn b() {} });
        cs.add_mod("client", m);
        let out = cs.into_stream().to_string();
        assert_eq!(out.matches("mod client").count(), 1);
        assert!(out.contains("fn a"));
        assert!(out.contains("fn b"));
    }

    #[test]
    #[should_panic(expected = "is a Rust keyword")]
    fn add_mod_keyword_panics() {
        let mut cs = Codespace::default();
        cs.add_mod("type", Mod::default());
    }

    #[test]
    fn into_files_adaptive_layout() {
        let mut cs = Codespace::default();
        cs.add_item("Root", quote! { pub struct Root; });
        // "a" is a leaf at the root.
        cs.add_item("a::f", quote! { pub fn f() {} });
        // "b" is a non-leaf at the root; "c" is a nested non-leaf; "d" is a
        // nested leaf.
        cs.add_item("b::g", quote! { pub fn g() {} });
        cs.add_item("b::c::h", quote! { pub fn h() {} });
        cs.add_item("b::c::d::i", quote! { pub fn i() {} });
        let files = cs.into_files();
        // Compare as Paths, not strings: Path equality is component-based,
        // so this holds on Windows where join produces backslashes.
        let paths = files.keys().map(PathBuf::as_path).collect::<Vec<_>>();
        assert_eq!(
            paths,
            ["a.rs", "b/c/d.rs", "b/c/mod.rs", "b/mod.rs", "lib.rs"].map(Path::new)
        );

        let lib = no_ws(&files[Path::new("lib.rs")].to_string());
        assert!(lib.contains("pubstructRoot"));
        assert!(lib.contains("pubmoda;"));
        assert!(lib.contains("pubmodb;"));
        // Declarations sit at the top of the file, alphabetically, with
        // items after them.
        let root_pos = lib.find("pubstructRoot").unwrap();
        let a_pos = lib.find("pubmoda;").unwrap();
        let b_pos = lib.find("pubmodb;").unwrap();
        assert!(a_pos < b_pos && b_pos < root_pos);

        let a = no_ws(&files[Path::new("a.rs")].to_string());
        assert!(a.contains("pubfnf()"));
        assert!(!a.contains("mod"));

        let b = no_ws(&files[Path::new("b/mod.rs")].to_string());
        assert!(b.contains("pubfng()"));
        assert!(b.contains("pubmodc;"));
        assert!(b.find("pubmodc;").unwrap() < b.find("pubfng()").unwrap());

        let c = no_ws(&files[Path::new("b/c/mod.rs")].to_string());
        assert!(c.contains("pubfnh()"));
        assert!(c.contains("pubmodd;"));

        let d = no_ws(&files[Path::new("b/c/d.rs")].to_string());
        assert!(d.contains("pubfni()"));
    }

    #[test]
    fn into_files_decl_carries_metadata() {
        let mut cs = Codespace::default();
        let m = cs.get_root_mod().get_mod("helpers");
        m.set_visibility(Visibility::Crate);
        m.add_docs("Helper functions.");
        m.add_attr(quote! { allow(dead_code) });
        m.add_item("f", quote! { pub fn f() {} });
        let files = cs.into_files();
        let lib = no_ws(&files[Path::new("lib.rs")].to_string());
        // Docs and attrs are outer, at the declaration site, immediately
        // before the visibility and declaration.
        assert!(
            lib.contains(r##"#[doc="Helperfunctions."]#[allow(dead_code)]pub(crate)modhelpers;"##)
        );
        // Nothing of the metadata leaks into the child file.
        let helpers = no_ws(&files[Path::new("helpers.rs")].to_string());
        assert!(!helpers.contains("doc"));
        assert!(!helpers.contains("allow"));
    }

    #[test]
    fn into_files_root_meta_is_inner() {
        let mut cs = Codespace::default();
        cs.get_root_mod().add_docs("Generated code.");
        cs.get_root_mod().add_attr(quote! { allow(clippy::all) });
        cs.add_item("Foo", quote! { pub struct Foo; });
        let files = cs.into_files();
        let lib = no_ws(&files[Path::new("lib.rs")].to_string());
        assert!(lib.starts_with(r##"#![doc="Generatedcode."]#![allow(clippy::all)]"##));
    }

    #[test]
    fn into_files_itemless_parent_mod() {
        let mut cs = Codespace::default();
        // "outer" has no items of its own, only a child module.
        cs.add_item("outer::inner::f", quote! { pub fn f() {} });
        let files = cs.into_files();
        // Path comparison for Windows separator tolerance; see
        // into_files_adaptive_layout.
        let paths = files.keys().map(PathBuf::as_path).collect::<Vec<_>>();
        assert_eq!(
            paths,
            ["lib.rs", "outer/inner.rs", "outer/mod.rs"].map(Path::new)
        );

        // The itemless parent's mod.rs holds exactly the declaration.
        let outer = no_ws(&files[Path::new("outer/mod.rs")].to_string());
        assert_eq!(outer, "pubmodinner;");

        let inner = no_ws(&files[Path::new("outer/inner.rs")].to_string());
        assert!(inner.contains("pubfnf()"));
    }

    #[test]
    fn into_files_empty_codespace() {
        let files = Codespace::default().into_files();
        assert_eq!(files.len(), 1);
        assert!(files[Path::new("lib.rs")].is_empty());
    }

    #[test]
    fn add_docs_appends_as_paragraphs() {
        let mut cs = Codespace::default();
        let m = cs.get_root_mod().get_mod("helpers");
        m.add_docs("First paragraph.\nStill the first.");
        m.add_docs("Second paragraph.");
        m.add_item("f", quote! { pub fn f() {} });
        let out = no_ws(&cs.into_stream().to_string());
        // Lines within one call stay in one paragraph; a blank doc line
        // separates calls.
        assert!(out.contains(
            r##"#[doc="Firstparagraph."]#[doc="Stillthefirst."]#[doc=""]#[doc="Secondparagraph."]"##
        ));
    }

    #[test]
    fn root_docs_and_attrs_are_inner() {
        let mut cs = Codespace::default();
        cs.get_root_mod().add_docs("Generated code.\nDo not edit.");
        cs.get_root_mod().add_attr(quote! { allow(clippy::all) });
        cs.add_item("Foo", quote! { pub struct Foo; });
        let out = no_ws(&cs.into_stream().to_string());
        assert!(out.starts_with(
            r##"#![doc="Generatedcode."]#![doc="Donotedit."]#![allow(clippy::all)]"##
        ));
        assert!(out.contains("pubstructFoo"));
    }
}
