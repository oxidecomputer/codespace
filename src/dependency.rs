// Copyright 2026 Oxide Computer Company

//! The external crates generated code needs.

use semver::VersionReq;

/// One line of a `[dependencies]` table: a crate the generated code
/// refers to and how a consumer should declare it.
///
/// A generator registers one through
/// [`Codespace::add_dependency`](crate::Codespace::add_dependency);
/// [`Codespace::dependencies`](crate::Codespace::dependencies) answers
/// them all.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dependency {
    name: String,
    rename: Option<String>,
    version: VersionReq,
    features: Vec<String>,
}

impl Dependency {
    /// A dependency on the crate `name` at any version, with no features.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            rename: None,
            version: VersionReq::STAR,
            features: Vec::new(),
        }
    }

    /// The identifier generated code uses for the crate when it differs
    /// from the crate's name; a consumer declares it as
    /// `rename = { package = "name", ... }`. Two versions of one crate
    /// coexist under two renames (`schemars08` and `schemars1`, say).
    /// A rename equal to the identifier the name already answers is
    /// no rename.
    pub fn with_rename(mut self, rename: impl Into<String>) -> Self {
        let rename = rename.into();
        self.rename = (rename != self.name.replace('-', "_")).then_some(rename);
        self
    }

    /// The version requirement, as Cargo reads it.
    pub fn with_version(mut self, version: VersionReq) -> Self {
        self.version = version;
        self
    }

    /// Features the generated code needs enabled; kept sorted and
    /// without duplicates.
    pub fn with_features(mut self, features: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.features.extend(features.into_iter().map(Into::into));
        self.features.sort();
        self.features.dedup();
        self
    }

    /// The crate's name as a registry knows it.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The rename, if the generated code refers to the crate by another
    /// identifier.
    pub fn rename(&self) -> Option<&str> {
        self.rename.as_deref()
    }

    /// The version requirement; [`VersionReq::STAR`] when any version
    /// will do.
    pub fn version(&self) -> &VersionReq {
        &self.version
    }

    /// The features the generated code needs, sorted.
    pub fn features(&self) -> &[String] {
        &self.features
    }

    /// The identifier generated code uses for this crate: the rename if
    /// there is one, otherwise the name with `-` as `_`, which is how
    /// Cargo exposes a crate to code.
    pub fn ident(&self) -> String {
        match &self.rename {
            Some(rename) => rename.clone(),
            None => self.name.replace('-', "_"),
        }
    }

    /// Fold a second registration under the same identifier into this
    /// one.
    ///
    /// The two must name the same crate. Features are unioned. A
    /// version of `*` yields to any other requirement; two other
    /// requirements must be equal.
    pub(crate) fn merge(&mut self, other: Dependency) -> Result<(), DependencyConflict> {
        debug_assert_eq!(self.ident(), other.ident());
        if self.name != other.name {
            return Err(DependencyConflict {
                ident: self.ident(),
                reason: format!(
                    "the crate `{}` in one place and `{}` in another",
                    self.name, other.name
                ),
            });
        }
        match (&self.version, &other.version) {
            (_, v) if *v == VersionReq::STAR => {}
            (v, _) if *v == VersionReq::STAR => self.version = other.version,
            (a, b) if a == b => {}
            (a, b) => {
                return Err(DependencyConflict {
                    ident: self.ident(),
                    reason: format!("version {a} in one place and {b} in another"),
                });
            }
        }
        self.features.extend(other.features);
        self.features.sort();
        self.features.dedup();
        Ok(())
    }
}

/// Two registrations under one identifier that cannot both be honored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DependencyConflict {
    /// The identifier the code uses; see [`Dependency::ident`].
    pub ident: String,
    /// What disagreed.
    pub reason: String,
}

impl std::fmt::Display for DependencyConflict {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "conflicting dependency `{}`: {}",
            self.ident, self.reason
        )
    }
}

impl std::error::Error for DependencyConflict {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ident_follows_cargo_naming() {
        assert_eq!(Dependency::new("json-serde").ident(), "json_serde");
        assert_eq!(Dependency::new("serde_json").ident(), "serde_json");
        assert_eq!(
            Dependency::new("json-serde").with_rename("js").ident(),
            "js"
        );
    }

    #[test]
    fn features_are_sorted_and_unique() {
        let dep = Dependency::new("uuid").with_features(["v4", "serde", "v4"]);
        assert_eq!(dep.features(), ["serde", "v4"]);
    }

    #[test]
    fn merge_unions_features_and_lets_star_yield() {
        let mut dep = Dependency::new("uuid").with_features(["serde"]);
        dep.merge(
            Dependency::new("uuid")
                .with_version("1.0".parse().unwrap())
                .with_features(["v4"]),
        )
        .unwrap();
        assert_eq!(dep.version(), &"1.0".parse::<VersionReq>().unwrap());
        assert_eq!(dep.features(), ["serde", "v4"]);

        // The other direction yields the same way.
        dep.merge(Dependency::new("uuid")).unwrap();
        assert_eq!(dep.version(), &"1.0".parse::<VersionReq>().unwrap());
    }

    #[test]
    fn merge_refuses_different_versions() {
        let mut dep = Dependency::new("uuid").with_version("1.0".parse().unwrap());
        let err = dep
            .merge(Dependency::new("uuid").with_version("2.0".parse().unwrap()))
            .unwrap_err();
        assert_eq!(err.ident, "uuid");
        assert!(err.reason.contains("version"), "{err}");
    }

    #[test]
    fn merge_refuses_different_crates_under_one_identifier() {
        let mut dep = Dependency::new("json-serde").with_rename("js");
        let err = dep
            .merge(Dependency::new("other-serde").with_rename("js"))
            .unwrap_err();
        assert_eq!(err.ident, "js");
        assert!(err.reason.contains("`json-serde`"), "{err}");
    }

    #[test]
    fn a_rename_to_the_default_identifier_is_no_rename() {
        let dep = Dependency::new("json-serde").with_rename("json_serde");
        assert_eq!(dep.rename(), None);
        assert_eq!(dep.ident(), "json_serde");
    }
}
