pub(crate) mod edit;
mod schema;
pub(crate) mod select;

use std::path::Path;

pub(crate) use schema::{
    BuildScript, Dependency, DependencyKind, Edition, GitSource, Manifest, ProfileEntry,
    TargetSpec, Workspace as WorkspaceConfig,
};

impl Manifest {
    pub(crate) fn parse(input: &str, path: &Path) -> Result<Manifest, String> {
        let table =
            crate::toml::parse(input).map_err(|error| format!("{}: {error}", path.display()))?;
        schema::map(&table).map_err(|error| format!("{}: {error}", path.display()))
    }

    pub(crate) fn unsupported_features(&self) -> Vec<String> {
        // Git dependencies, patch overrides, and replace entries are all
        // supported now.
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::Manifest;
    use crate::manifest::schema::{BuildScript, Edition, PublishPolicy};

    const MANIFESTS: &[(&str, &str)] = &[
        (
            "cfg-if",
            include_str!("../../tests/fixtures/manifests/cfg-if-1.0.5.toml"),
        ),
        (
            "anyhow",
            include_str!("../../tests/fixtures/manifests/anyhow-1.0.104.toml"),
        ),
        (
            "serde",
            include_str!("../../tests/fixtures/manifests/serde-1.0.229.toml"),
        ),
        (
            "syn",
            include_str!("../../tests/fixtures/manifests/syn-3.0.9.toml"),
        ),
        (
            "clap",
            include_str!("../../tests/fixtures/manifests/clap-4.6.7.toml"),
        ),
    ];

    fn parse_manifest(input: &str) -> Manifest {
        Manifest::parse(input, Path::new("Cargo.toml")).unwrap()
    }

    fn error(input: &str) -> String {
        Manifest::parse(input, Path::new("Cargo.toml")).unwrap_err()
    }

    #[test]
    fn parses_package_fields() {
        let manifest = parse_manifest(
            "[package]\nname = \"demo\"\nversion = \"0.1.0\"\nedition = \"2021\"\nauthors = [\"a\", \"b\"]\nlicense = \"MIT\"\npublish = false\nbuild = \"build.rs\"\nrust-version = \"1.70\"\n",
        );

        assert_eq!(manifest.name, "demo");
        assert_eq!(manifest.version, "0.1.0");
        assert_eq!(manifest.edition, Edition::E2021);
        assert_eq!(manifest.edition.as_str(), "2021");
        assert_eq!(manifest.authors, vec!["a", "b"]);
        assert_eq!(manifest.license.as_deref(), Some("MIT"));
        assert_eq!(manifest.publish, Some(PublishPolicy::Never));
        assert_eq!(manifest.build, Some(BuildScript::Path("build.rs".into())));
        assert_eq!(manifest.rust_version.as_deref(), Some("1.70"));

        let manifest = parse_manifest("[package]\nname = \"demo\"\nversion = \"0.1.0\"\n");
        assert_eq!(manifest.edition, Edition::E2015);
        assert_eq!(manifest.build, None);

        let manifest =
            parse_manifest("[package]\nname = \"demo\"\nversion = \"0.1.0\"\nbuild = false\n");
        assert_eq!(manifest.build, Some(BuildScript::Disabled));
    }

    #[test]
    fn parses_dependencies_and_features() {
        let manifest = parse_manifest(
            "[package]\nname = \"demo\"\nversion = \"0.1.0\"\n\n[dependencies]\nserde = { version = \"1\", optional = true, features = [\"derive\"], default-features = false }\nanyhow = \"1\"\n\n[features]\ndefault = [\"std\"]\nfull = [\"dep:serde\", \"serde/derive\"]\n",
        );

        let serde = manifest.dependencies.get("serde").unwrap();
        assert_eq!(serde.requirement.as_deref(), Some("1"));
        assert!(serde.optional);
        assert!(!serde.default_features.unwrap_or(true));
        assert_eq!(serde.features, vec!["derive"]);

        assert_eq!(
            manifest.dependencies.get("anyhow").unwrap().requirement,
            Some("1".into())
        );

        assert_eq!(
            manifest.features.get("default"),
            Some(&vec!["std".to_string()])
        );
        assert_eq!(
            manifest.features.get("full"),
            Some(&vec!["dep:serde".to_string(), "serde/derive".to_string()])
        );
    }

    #[test]
    fn parses_targets_and_workspace() {
        let manifest = parse_manifest(
            "[package]\nname = \"demo\"\nversion = \"0.1.0\"\n\n[lib]\nname = \"demo\"\npath = \"src/lib.rs\"\n\n[[bin]]\nname = \"a\"\npath = \"src/a.rs\"\n\n[[bin]]\nname = \"b\"\n\n[target.'cfg(unix)'.dependencies]\nlibc = \"0.2\"\n\n[workspace]\nmembers = [\"crates/a\", \"crates/*\"]\nexclude = [\"old\"]\nresolver = \"2\"\n",
        );

        let lib = manifest.lib.as_ref().unwrap();
        assert_eq!(lib.name.as_deref(), Some("demo"));
        assert_eq!(lib.path.as_deref(), Some("src/lib.rs"));

        assert_eq!(manifest.bins.len(), 2);
        assert_eq!(manifest.bins[1].name.as_deref(), Some("b"));

        let target = manifest.targets.get("cfg(unix)").unwrap();
        assert_eq!(
            target.dependencies.get("libc").unwrap().requirement,
            Some("0.2".into())
        );

        let workspace = manifest.workspace.as_ref().unwrap();
        assert_eq!(workspace.members, vec!["crates/a", "crates/*"]);
        assert_eq!(workspace.exclude, vec!["old"]);
        assert_eq!(workspace.resolver.as_deref(), Some("2"));
    }

    #[test]
    fn parses_profiles() {
        let manifest = parse_manifest(
            "[package]\nname = \"demo\"\nversion = \"0.1.0\"\n\n[profile.release]\nopt-level = 3\nlto = \"thin\"\nstrip = \"symbols\"\npanic = \"abort\"\n\n[profile.dev.package.\"*\"]\nopt-level = 1\n",
        );

        let release = manifest.profiles.get("release").unwrap();
        assert!(release
            .settings
            .opt_level
            .is_some_and(|level| level == crate::profile::OptLevel::O3));
        assert!(release
            .settings
            .lto
            .is_some_and(|lto| lto == crate::profile::Lto::Thin));
        assert!(release
            .settings
            .strip
            .is_some_and(|strip| strip == crate::profile::Strip::Symbols));
        assert!(release
            .settings
            .panic
            .is_some_and(|panic| panic == crate::profile::Panic::Abort));

        let dev = manifest.profiles.get("dev").unwrap();
        assert!(dev.package.contains_key("*"));
        assert_eq!(dev.package.len(), 1);
    }

    #[test]
    fn invalid_field_types_fail() {
        assert!(
            error("dependencies = 1\n[package]\nname = \"demo\"\nversion = \"0.1.0\"\n")
                .contains("`[dependencies]` must be a table")
        );
        assert!(
            error("[package]\nname = \"demo\"\nversion = \"0.1.0\"\n\n[dependencies]\nx = { optional = \"yes\" }\n")
                .contains("`dependencies.x.optional` must be a boolean")
        );
        assert!(error(
            "[package]\nname = \"demo\"\nversion = \"0.1.0\"\n\n[profile.release]\nopt-level = 9\n"
        )
        .contains("`opt-level`"));
        assert!(
            error("[package]\nname = \"demo\"\nversion = \"0.2.0\"\nedition = \"2019\"\n")
                .contains("unsupported edition")
        );
    }

    #[test]
    fn unknown_keys_become_warnings() {
        let manifest = parse_manifest(
            "[package]\nname = \"demo\"\nversion = \"0.1.0\"\nnonsense = 1\n\n[dependencies]\nserde = { version = \"1\", mystery = true }\n",
        );

        assert!(manifest
            .warnings
            .iter()
            .any(|warning| warning.contains("unknown key `nonsense`")));
        assert!(manifest
            .warnings
            .iter()
            .any(|warning| warning.contains("mystery")));
    }

    #[test]
    fn editions_round_trip() {
        for edition in ["2015", "2018", "2021", "2024"] {
            let manifest = parse_manifest(&format!(
                "[package]\nname = \"demo\"\nversion = \"0.1.0\"\nedition = \"{edition}\"\n"
            ));
            assert_eq!(manifest.edition.as_str(), edition);
        }
    }

    #[test]
    fn includes_the_manifest_path_in_errors() {
        let error = Manifest::parse("nope", Path::new("rusted.toml"))
            .err()
            .unwrap();
        assert!(error.starts_with("rusted.toml: "), "{error}");
        assert!(error.contains("line 1"), "{error}");
    }

    #[test]
    fn parses_real_manifests() {
        for (name, input) in MANIFESTS {
            let manifest = Manifest::parse(input, Path::new("Cargo.toml"))
                .unwrap_or_else(|error| panic!("{name}: {error}"));
            assert!(!manifest.name.is_empty(), "{name}");
            assert!(!manifest.version.is_empty(), "{name}");
        }
    }
}
