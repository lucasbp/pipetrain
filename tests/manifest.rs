use std::collections::BTreeSet;

use pipetrain::manifest::{published, references};

fn names(found: BTreeSet<String>) -> Vec<String> {
    found.into_iter().collect()
}

#[test]
fn explicit_package_id_wins() {
    let text = "<PackageId>Acme.Util</PackageId>";
    assert_eq!(
        published("src/Acme.Util.csproj", text).as_deref(),
        Some("acme.util")
    );
}

#[test]
fn generate_package_on_build_publishes_the_file_stem() {
    let text = "<GeneratePackageOnBuild>true</GeneratePackageOnBuild>";
    assert_eq!(
        published("src/Acme.Util.csproj", text).as_deref(),
        Some("acme.util")
    );
}

#[test]
fn assembly_name_beats_the_file_stem() {
    let text = "<IsPackable>true</IsPackable><AssemblyName>Acme.Core</AssemblyName>";
    assert_eq!(
        published("src/Util.csproj", text).as_deref(),
        Some("acme.core")
    );
}

#[test]
fn variable_assembly_name_falls_back_to_the_stem() {
    let text = "<IsPackable>true</IsPackable><AssemblyName>$(Root).Core</AssemblyName>";
    assert_eq!(
        published("src/Acme.Core.csproj", text).as_deref(),
        Some("acme.core")
    );
}

#[test]
fn a_plain_project_publishes_nothing() {
    let text = "<TargetFramework>net9.0</TargetFramework>";
    assert_eq!(published("src/Acme.App.csproj", text), None);
}

#[test]
fn not_packable_is_excluded_even_with_a_package_id() {
    let text = "<PackageId>Acme.Util</PackageId><IsPackable>false</IsPackable>";
    assert_eq!(published("src/Acme.Util.csproj", text), None);
}

#[test]
fn test_projects_are_excluded() {
    let text = "<GeneratePackageOnBuild>true</GeneratePackageOnBuild>";
    assert_eq!(published("tests/Acme.Util.Tests.csproj", text), None);
    assert_eq!(published("tests/Acme.Util.Test.csproj", text), None);
    let sdk =
        "<IsPackable>true</IsPackable><PackageReference Include=\"Microsoft.NET.Test.Sdk\" />";
    assert_eq!(published("tests/Acme.Checks.csproj", sdk), None);
}

#[test]
fn variable_package_id_still_falls_back_to_the_stem() {
    let text = "<PackageId>$(Prefix).Util</PackageId>";
    assert_eq!(
        published("src/Acme.Util.csproj", text).as_deref(),
        Some("acme.util")
    );
}

#[test]
fn package_json_publisher_is_unchanged() {
    assert_eq!(
        published("package.json", r#"{"name":"@Acme/Sdk"}"#).as_deref(),
        Some("@acme/sdk")
    );
    assert_eq!(
        published("package.json", r#"{"name":"acme","private":true}"#),
        None
    );
}

#[test]
fn csproj_references_cover_include_and_update() {
    let text = r#"<ItemGroup>
  <PackageReference Include="Acme.Util" Version="1.0.0" />
  <PackageReference Update="Acme.Core" Version="2.0.0" />
  <PackageVersion Include="Newtonsoft.Json" Version="13.0.1" />
  <PackageReference Include="$(Dynamic)" />
</ItemGroup>"#;
    assert_eq!(
        names(references("src/App.csproj", text)),
        vec!["acme.core", "acme.util", "newtonsoft.json"]
    );
}

#[test]
fn props_references_are_read() {
    let text = r#"<PackageVersion Include="Acme.Util" Version="1.0.0" />"#;
    assert_eq!(
        names(references("Directory.Packages.props", text)),
        vec!["acme.util"]
    );
}

#[test]
fn references_ignore_attribute_order_and_case() {
    let text = r#"<packagereference Version="1.0" INCLUDE="Acme.Util"/>"#;
    assert_eq!(names(references("a.csproj", text)), vec!["acme.util"]);
}

#[test]
fn npm_references_cover_the_three_sections() {
    let text = r#"{"name":"acme-web","dependencies":{"@Acme/Sdk":"^1.0.0"},"devDependencies":{"vite":"^5"},"peerDependencies":{"react":"^18"},"scripts":{"build":"x"}}"#;
    assert_eq!(
        names(references("web/package.json", text)),
        vec!["@acme/sdk", "react", "vite"]
    );
}

#[test]
fn broken_json_and_other_files_reference_nothing() {
    assert!(references("package.json", "{").is_empty());
    assert!(references("README.md", "Include=\"Acme.Util\"").is_empty());
}
