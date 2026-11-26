use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use proc_macro2::Span;
use syn::ext::IdentExt;
use syn::punctuated::Punctuated;
use syn::spanned::Spanned;
use syn::visit::{self, Visit};
use syn::{Attribute, Block, Item, ItemMod, ItemUse, Stmt, UseTree};

fn test_only(attrs: &[Attribute]) -> bool {
    attrs.iter().any(|attr| {
        attr.path().is_ident("test")
            || (attr.path().is_ident("cfg")
                && attr.parse_args::<syn::Ident>().is_ok_and(|id| id == "test"))
    })
}

fn production(item: &Item) -> bool {
    macro_rules! attributes {
        ($($kind:ident),*) => {
            match item {
                $(Item::$kind(item) => &item.attrs[..],)*
                _ => &[],
            }
        };
    }
    !test_only(attributes!(
        Const,
        Enum,
        ExternCrate,
        Fn,
        ForeignMod,
        Impl,
        Macro,
        Mod,
        Static,
        Struct,
        Trait,
        TraitAlias,
        Type,
        Union,
        Use
    ))
}

fn imports(tree: &UseTree, prefix: &str, found: &mut impl FnMut(String, String, Span)) {
    let join = |name: &syn::Ident| {
        if name == "self" && !prefix.is_empty() {
            prefix.to_owned()
        } else if prefix.is_empty() {
            name.unraw().to_string()
        } else {
            format!("{prefix}::{}", name.unraw())
        }
    };
    match tree {
        UseTree::Path(path) => imports(&path.tree, &join(&path.ident), found),
        UseTree::Group(group) => {
            for item in &group.items {
                imports(item, prefix, found);
            }
        }
        UseTree::Name(name) => {
            let path = join(&name.ident);
            let binding = path.rsplit("::").next().unwrap().to_owned();
            found(path, binding, name.span());
        }
        UseTree::Rename(rename) => found(
            join(&rename.ident),
            rename.rename.unraw().to_string(),
            rename.span(),
        ),
        UseTree::Glob(glob) => found(format!("{prefix}::*"), "*".to_owned(), glob.span()),
    }
}

fn boundary(path: &str) -> Option<&'static str> {
    let parts: Vec<_> = path.split("::").collect();
    match parts.as_slice() {
        ["crate", "command", ..] | ["crate", "*"] => {
            Some("keep CLI orchestration in command and pass domain inputs here")
        }
        ["ureq", "http" | "Error" | "Body", ..] => None,
        ["crate", "feed", "*"] | ["std", "*"] => {
            Some("use explicit imports so network dependencies can be checked")
        }
        ["crate", "github", "Client" | "*", ..]
        | [
            "crate",
            "feed",
            "registry",
            "Session" | "Checker" | "fetch" | "exclude_go_retractions" | "*",
            ..,
        ]
        | ["ureq", _, ..]
        | [
            "std",
            "net",
            "TcpStream" | "UdpSocket" | "TcpListener" | "ToSocketAddrs" | "*",
            ..,
        ] => Some("perform network I/O in an adapter and pass the resulting data here"),
        _ => None,
    }
}

struct Guard<'a> {
    module: String,
    file: PathBuf,
    directory: PathBuf,
    aliases: BTreeMap<String, String>,
    errors: &'a mut Vec<String>,
}

impl Guard<'_> {
    fn qualify(&self, path: &str) -> String {
        let mut parts = path.split("::").peekable();
        let mut root = self.module.as_str();
        match parts.peek() {
            Some(&"crate" | &"pipetrain") => {
                root = "crate";
                parts.next();
            }
            Some(&"self") => {
                parts.next();
            }
            Some(&"super") => {
                while parts.next_if_eq(&"super").is_some() {
                    root = root.rsplit_once("::").map_or("crate", |(parent, _)| parent);
                }
            }
            Some(&"std" | &"ureq") => return path.to_owned(),
            _ => {}
        }
        std::iter::once(root)
            .chain(parts)
            .collect::<Vec<_>>()
            .join("::")
    }

    fn resolve(&self, path: &str) -> String {
        let mut path = self.qualify(path);
        let mut seen = BTreeSet::new();
        while let Some((prefix, target)) = self.aliases.iter().rev().find(|(prefix, _)| {
            path == **prefix
                || path
                    .strip_prefix(prefix.as_str())
                    .is_some_and(|rest| rest.starts_with("::"))
        }) {
            if !seen.insert(prefix) {
                break;
            }
            path = format!("{target}{}", &path[prefix.len()..]);
        }
        path
    }

    fn inspect(&mut self, path: &str, span: Span) {
        let path = self.resolve(path);
        if let Some(reason) = boundary(&path) {
            self.errors.push(format!(
                "{}:{}: {} uses {path}: {reason}",
                self.file.display(),
                span.start().line,
                self.module
            ));
        }
    }

    fn bind(&mut self, item: &Item) {
        if !production(item) {
            return;
        }
        if let Item::Use(item) = item {
            imports(&item.tree, "", &mut |path, name, _| {
                if let Some(namespace) = path.strip_suffix("::*") {
                    let prefix = format!("{}::", self.resolve(namespace));
                    let inherited: Vec<_> = self
                        .aliases
                        .iter()
                        .filter_map(|(key, target)| {
                            let member = key.strip_prefix(&prefix)?;
                            (!member.contains("::"))
                                .then(|| (format!("{}::{member}", self.module), target.clone()))
                        })
                        .collect();
                    for (alias, target) in inherited {
                        self.aliases.entry(alias).or_insert(target);
                    }
                } else {
                    self.aliases
                        .insert(format!("{}::{name}", self.module), self.qualify(&path));
                }
            });
        } else if let Item::ExternCrate(item) = item
            && let Some((_, alias)) = &item.rename
        {
            self.aliases.insert(
                format!("{}::{}", self.module, alias.unraw()),
                self.qualify(&item.ident.unraw().to_string()),
            );
        }
    }

    fn items(&mut self, items: &[Item]) {
        for item in items {
            self.bind(item);
        }
        for item in items {
            self.visit_item(item);
        }
    }

    fn read(&mut self) {
        let source = fs::read_to_string(&self.file)
            .unwrap_or_else(|e| panic!("{}: {e}", self.file.display()));
        let file =
            syn::parse_file(&source).unwrap_or_else(|e| panic!("{}: {e}", self.file.display()));
        if !test_only(&file.attrs) {
            self.items(&file.items);
        }
    }
}

impl<'ast> Visit<'ast> for Guard<'_> {
    fn visit_item(&mut self, item: &'ast Item) {
        if production(item) {
            visit::visit_item(self, item);
        }
    }

    fn visit_item_mod(&mut self, item: &'ast ItemMod) {
        let name = item.ident.unraw().to_string();
        let directory = self.directory.join(&name);
        let mut child = Guard {
            module: format!("{}::{name}", self.module),
            file: self.file.clone(),
            directory: directory.clone(),
            aliases: self.aliases.clone(),
            errors: self.errors,
        };
        if let Some((_, items)) = &item.content {
            child.items(items);
        } else {
            assert!(
                !item.attrs.iter().any(|attr| attr.path().is_ident("path")),
                "{}:{}: cannot resolve a custom module path for {name}. Extend the architecture check to follow it.",
                self.file.display(),
                item.ident.span().start().line
            );
            let flat = self.directory.join(format!("{name}.rs"));
            child.file = if flat.exists() {
                flat
            } else {
                directory.join("mod.rs")
            };
            child.read();
        }
    }

    fn visit_block(&mut self, block: &'ast Block) {
        let saved = self.aliases.clone();
        for stmt in &block.stmts {
            if let Stmt::Item(item) = stmt {
                self.bind(item);
            }
        }
        visit::visit_block(self, block);
        self.aliases = saved;
    }

    fn visit_item_use(&mut self, item: &'ast ItemUse) {
        imports(&item.tree, "", &mut |path, _, span| {
            self.inspect(&path, span)
        });
    }

    fn visit_impl_item_fn(&mut self, item: &'ast syn::ImplItemFn) {
        if !test_only(&item.attrs) {
            visit::visit_impl_item_fn(self, item);
        }
    }

    fn visit_macro(&mut self, mac: &'ast syn::Macro) {
        self.visit_path(&mac.path);
        if let Ok(args) =
            mac.parse_body_with(Punctuated::<syn::Expr, syn::Token![,]>::parse_terminated)
        {
            for arg in &args {
                self.visit_expr(arg);
            }
        }
    }

    fn visit_path(&mut self, path: &'ast syn::Path) {
        let names: Vec<_> = path
            .segments
            .iter()
            .map(|s| s.ident.unraw().to_string())
            .collect();
        self.inspect(&names.join("::"), path.span());
        visit::visit_path(self, path);
    }
}

#[test]
fn scoped_aliases_keep_data_types_and_reject_client_and_command_paths() {
    assert!(boundary("crate::feed::registry::exclude_go_retractions").is_some());
    assert!(boundary("crate::feed::Coordinate").is_none());
    let file = syn::parse_file(
        r#"
        use crate::{github as gh, github::Run};
        fn render(_: Run) {
            use gh::Client as Network;
            let _ = |_: &Network| "crate::command::fake";
        }
        mod child {
            use super::*;
            use super::super::command::bump;
            fn unexpected(_: &gh::Client) {}
        }
        #[cfg(test)]
        mod tests { use crate::command::feed; }
    "#,
    )
    .unwrap();
    let mut errors = Vec::new();
    Guard {
        module: "crate::render".to_owned(),
        file: "render.rs".into(),
        directory: "render".into(),
        aliases: BTreeMap::new(),
        errors: &mut errors,
    }
    .items(&file.items);
    assert!(
        errors
            .iter()
            .any(|e| e.contains("crate::render::child uses crate::github::Client:"))
    );
    assert!(
        errors
            .iter()
            .any(|e| e.contains("uses crate::github::Client:"))
    );
    assert!(
        errors
            .iter()
            .any(|e| e.contains("uses crate::command::bump:"))
    );
    assert!(
        errors
            .iter()
            .all(|e| e.contains("uses crate::github::Client:")
                || e.contains("uses crate::command::bump:")),
        "{errors:?}"
    );
}

#[test]
fn production_domain_modules_respect_their_dependencies() {
    let mut errors = Vec::new();
    for module in ["planner", "plan", "merge/verdict", "render"] {
        let directory = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("src")
            .join(module);
        Guard {
            module: format!("crate::{}", module.replace('/', "::")),
            file: directory.with_extension("rs"),
            directory,
            aliases: BTreeMap::new(),
            errors: &mut errors,
        }
        .read();
    }
    assert!(errors.is_empty(), "\n{}", errors.join("\n"));
}
