// Copyright 2026 Oxide Computer Company

//! The external dependencies that generated code requires.

use semver::{Comparator, VersionReq};

/// One line of a `[dependencies]` table: a crate the generated code
/// refers to and how a consumer should declare it.
///
/// A generator registers one through
/// [`Codespace::add_dependency`](crate::Codespace::add_dependency);
/// [`Codespace::dependencies`](crate::Codespace::dependencies) answers
/// them all. [`Dependency::new`] is the common case; the rest is set by
/// struct update:
///
/// ```rust
/// use codespace::Dependency;
///
/// let chrono = Dependency {
///     version: "0.4".parse().unwrap(),
///     features: vec!["serde".to_string()],
///     default_features: Some(false),
///     ..Dependency::new("chrono")
/// };
/// assert_eq!(
///     chrono.to_toml_inline(),
///     r#"chrono = { version = "0.4", default-features = false, features = ["serde"] }"#
/// );
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dependency {
    /// The crate's name as a registry knows it.
    pub name: String,
    /// The identifier generated code uses for the crate when it differs
    /// from the crate's name.
    pub rename: Option<String>,
    /// The version requirement, as Cargo reads it; [`VersionReq::STAR`]
    /// when any version will do.
    pub version: VersionReq,
    /// Features the generated code needs enabled, in any order.
    pub features: Vec<String>,
    /// Whether the crate's default features are needed; a consumer writes
    /// `default-features = false` for `Some(false)`.
    pub default_features: Option<bool>,
}

impl Dependency {
    /// A dependency on the crate `name` at any version, with no features.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            rename: None,
            version: VersionReq::STAR,
            features: Vec::new(),
            default_features: None,
        }
    }

    /// The identifier generated code uses for this crate: the rename if there
    /// is one, otherwise the name with `-` as `_`, which is how Cargo exposes
    /// a crate to code.
    pub fn ident(&self) -> String {
        self.toml_key().replace('-', "_")
    }

    /// Fold a second registration under the same identifier into this
    /// one, as [`Codespace::add_dependency`](crate::Codespace::add_dependency)
    /// does.
    ///
    /// The caller has matched the identifiers; the two must name the same
    /// crate. Features are unioned, and so is the need for default
    /// features: one registration needing them is enough, and one leaving
    /// it unsaid yields. Versions intersect: `*` yields to any other
    /// requirement, two caret requirements in one compatibility range
    /// (`^1.5` and `^1.7`) merge to the higher (`^1.7`), and anything else
    /// must be equal.
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
        match intersect(&self.version, &other.version) {
            Some(version) => self.version = version,
            None => {
                return Err(DependencyConflict {
                    ident: self.ident(),
                    reason: format!(
                        "version {} in one place and {} in another",
                        self.version, other.version
                    ),
                });
            }
        }
        self.features.extend(other.features);
        self.features.sort();
        self.features.dedup();
        self.default_features = match (self.default_features, other.default_features) {
            (Some(a), Some(b)) => Some(a || b),
            (a, b) => a.or(b),
        };
        Ok(())
    }

    /// The crate as one line of a `[dependencies]` table.
    ///
    /// A bare requirement renders as `chrono = "0.4"`; anything more
    /// renders as an inline table, such as
    /// `schemars08 = { version = "0.8", package = "schemars", features = ["derive"] }`.
    pub fn to_toml_inline(&self) -> String {
        let fields = self.toml_fields();
        match fields.as_slice() {
            [version] if version.starts_with("version = ") => {
                format!("{} = {}", self.toml_key(), &version["version = ".len()..])
            }
            fields => format!("{} = {{ {} }}", self.toml_key(), fields.join(", ")),
        }
    }

    /// The crate as its own `[dependencies.<key>]` table, one field per
    /// line, ending in a newline.
    pub fn to_toml_table(&self) -> String {
        let mut out = format!("[dependencies.{}]\n", self.toml_key());
        for field in self.toml_fields() {
            out.push_str(&field);
            out.push('\n');
        }
        out
    }

    /// The key of the crate's dependency table: the rename, unless it is
    /// the identifier the name already answers, or the crate's name as
    /// the registry spells it, dashes and all.
    fn toml_key(&self) -> &str {
        match &self.rename {
            Some(rename) if *rename != self.name.replace('-', "_") => rename,
            _ => &self.name,
        }
    }

    /// The `key = value` fields of the crate's dependency table, in the
    /// order Cargo manifests conventionally list them.
    fn toml_fields(&self) -> Vec<String> {
        let mut fields = vec![format!("version = \"{}\"", toml_version(&self.version))];
        if self.toml_key() != self.name {
            fields.push(format!("package = \"{}\"", self.name));
        }
        if self.default_features == Some(false) {
            fields.push("default-features = false".to_string());
        }
        if !self.features.is_empty() {
            let mut features = self.features.clone();
            features.sort();
            features.dedup();
            let features = features
                .iter()
                .map(|feature| format!("\"{feature}\""))
                .collect::<Vec<_>>()
                .join(", ");
            fields.push(format!("features = [{features}]"));
        }
        fields
    }
}

fn intersect(a: &VersionReq, b: &VersionReq) -> Option<VersionReq> {
    if a == b {
        return Some(a.clone());
    }

    // '*' matches anything so the other one wins
    if *b == VersionReq::STAR {
        return Some(a.clone());
    }
    if *a == VersionReq::STAR {
        return Some(b.clone());
    }

    // More could be done to compare version requirements as those cases
    // emerge.

    None
}

/// A version as usually written in Cargo.toml: a lone caret requirement as the
/// bare version (`0.4`, `1.2.3`), since the caret is Cargo's default, and
/// anything else as semver renders it.
fn toml_version(version: &VersionReq) -> String {
    if let [Comparator {
        op: semver::Op::Caret,
        major,
        minor,
        patch,
        pre,
    }] = version.comparators.as_slice()
    {
        let mut out = major.to_string();
        if let Some(minor) = minor {
            out.push_str(&format!(".{minor}"));
            if let Some(patch) = patch {
                out.push_str(&format!(".{patch}"));
                if !pre.is_empty() {
                    out.push_str(&format!("-{pre}"));
                }
            }
        }
        out
    } else {
        version.to_string()
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

    fn req(s: &str) -> VersionReq {
        s.parse().unwrap()
    }

    fn strings<const N: usize>(items: [&str; N]) -> Vec<String> {
        items.iter().map(|item| item.to_string()).collect()
    }

    #[test]
    fn ident_follows_cargo_naming() {
        assert_eq!(Dependency::new("json-serde").ident(), "json_serde");
        assert_eq!(Dependency::new("serde_json").ident(), "serde_json");
        let renamed = Dependency {
            rename: Some("js".to_string()),
            ..Dependency::new("json-serde")
        };
        assert_eq!(renamed.ident(), "js");
    }

    #[test]
    fn a_rename_to_the_default_identifier_is_no_rename() {
        let dep = Dependency {
            rename: Some("json_serde".to_string()),
            ..Dependency::new("json-serde")
        };
        assert_eq!(dep.ident(), "json_serde");
        assert_eq!(dep.to_toml_inline(), "json-serde = \"*\"");
    }

    #[test]
    fn merge_unions_features_and_lets_star_yield() {
        let mut dep = Dependency {
            features: strings(["serde"]),
            ..Dependency::new("uuid")
        };
        dep.merge(Dependency {
            version: req("1.0"),
            features: strings(["v4", "serde"]),
            ..Dependency::new("uuid")
        })
        .unwrap();
        assert_eq!(dep.version, req("1.0"));
        assert_eq!(dep.features, strings(["serde", "v4"]));

        // The other direction yields the same way.
        dep.merge(Dependency::new("uuid")).unwrap();
        assert_eq!(dep.version, req("1.0"));
    }

    #[test]
    fn merge_unions_the_need_for_default_features() {
        let mut dep = Dependency::new("chrono");
        assert_eq!(dep.default_features, None);
        let said = |default_features| Dependency {
            default_features: Some(default_features),
            ..Dependency::new("chrono")
        };
        dep.merge(said(false)).unwrap();
        assert_eq!(dep.default_features, Some(false));
        dep.merge(said(true)).unwrap();
        assert_eq!(dep.default_features, Some(true));
        dep.merge(Dependency::new("chrono")).unwrap();
        assert_eq!(dep.default_features, Some(true));
    }

    #[test]
    fn toml_inline_is_bare_for_a_version_alone() {
        assert_eq!(
            Dependency::new("serde_json").to_toml_inline(),
            "serde_json = \"*\""
        );
        let at = |name: &str, version: &str| Dependency {
            version: req(version),
            ..Dependency::new(name)
        };
        assert_eq!(at("chrono", "0.4").to_toml_inline(), "chrono = \"0.4\"");
        assert_eq!(
            at("futures-core", "0.3.1").to_toml_inline(),
            "futures-core = \"0.3.1\""
        );
        assert_eq!(
            at("serde", ">=1.0, <2").to_toml_inline(),
            "serde = \">=1.0, <2\""
        );
    }

    #[test]
    fn toml_inline_is_a_table_for_anything_more() {
        let dep = Dependency {
            rename: Some("schemars08".to_string()),
            version: req("0.8"),
            features: strings(["derive"]),
            default_features: Some(false),
            ..Dependency::new("schemars")
        };
        assert_eq!(
            dep.to_toml_inline(),
            "schemars08 = { version = \"0.8\", package = \"schemars\", \
             default-features = false, features = [\"derive\"] }"
        );
    }

    #[test]
    fn toml_table_lists_one_field_per_line_with_features_sorted() {
        let dep = Dependency {
            version: req("1.0"),
            features: strings(["v4", "serde"]),
            ..Dependency::new("uuid")
        };
        assert_eq!(
            dep.to_toml_table(),
            "[dependencies.uuid]\nversion = \"1.0\"\nfeatures = [\"serde\", \"v4\"]\n"
        );
    }

    #[test]
    fn merge_intersects_caret_versions() {
        let at = |version: &str| Dependency {
            version: req(version),
            ..Dependency::new("uuid")
        };
        let merged = |a: &str, b: &str| {
            let mut dep = at(a);
            dep.merge(at(b)).map(|()| dep.version.to_string())
        };
        assert_eq!(merged("1.5", "1.7").unwrap(), "^1.7");
        assert_eq!(merged("1.7", "1.5").unwrap(), "^1.7");
        assert_eq!(merged("1", "1.2.3").unwrap(), "^1.2.3");
        assert_eq!(merged("0.4", "0.4.2").unwrap(), "^0.4.2");
        assert_eq!(merged("0.0.3", "0.0.3").unwrap(), "^0.0.3");
        // Different compatibility ranges do not intersect.
        assert!(merged("1.7", "2.0").is_err());
        assert!(merged("0.4", "0.5").is_err());
        assert!(merged("0.0.3", "0.0.4").is_err());
        // Other requirement forms must be equal.
        assert_eq!(merged("=1.2.3", "=1.2.3").unwrap(), "=1.2.3");
        assert!(merged("=1.2.3", "1.2").is_err());
        assert!(merged(">=1.0, <2", "1.5").is_err());
    }

    #[test]
    fn merge_refuses_different_versions() {
        let mut dep = Dependency {
            version: req("1.0"),
            ..Dependency::new("uuid")
        };
        let err = dep
            .merge(Dependency {
                version: req("2.0"),
                ..Dependency::new("uuid")
            })
            .unwrap_err();
        assert_eq!(err.ident, "uuid");
        assert!(err.reason.contains("version"), "{err}");
    }

    #[test]
    fn merge_refuses_different_crates_under_one_identifier() {
        let mut dep = Dependency {
            rename: Some("js".to_string()),
            ..Dependency::new("json-serde")
        };
        let err = dep
            .merge(Dependency {
                rename: Some("js".to_string()),
                ..Dependency::new("other-serde")
            })
            .unwrap_err();
        assert_eq!(err.ident, "js");
        assert!(err.reason.contains("`json-serde`"), "{err}");
    }
}
