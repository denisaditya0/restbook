//! Scoped variable resolution and non-strict preview.
//!
//! [`crate::template::resolve`] answers *what a request sends* and fails on an
//! unknown name; this module also answers *why* by carrying the winning scope.
//! Preview is non-strict — unknowns stay literal and are reported unresolved —
//! and precedence is Global, Collection, Environment, Request, Live.

use std::collections::HashMap;

use crate::model::Kv;
use crate::template::VarContext;

/// The layers a `{{name}}` can be answered from, lowest precedence first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum VarScope {
    Global,
    Collection,
    Environment,
    Request,
    Live,
}

impl VarScope {
    /// Human-readable name, used as-is in the UI.
    pub fn label(self) -> &'static str {
        match self {
            VarScope::Global => "Global",
            VarScope::Collection => "Collection",
            VarScope::Environment => "Environment",
            VarScope::Request => "Request",
            VarScope::Live => "Live",
        }
    }
}

/// How far a scope is from winning: `0` for the narrowest, increasing towards
/// the broadest. Exhaustive on purpose — a new scope must pick a place here.
fn precedence_rank(scope: VarScope) -> usize {
    match scope {
        VarScope::Live => 0,
        VarScope::Request => 1,
        VarScope::Environment => 2,
        VarScope::Collection => 3,
        VarScope::Global => 4,
    }
}

/// Every scope in precedence order, lowest first.
const PRECEDENCE: [VarScope; 5] = [
    VarScope::Global,
    VarScope::Collection,
    VarScope::Environment,
    VarScope::Request,
    VarScope::Live,
];

/// A resolved variable together with the scope it came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedVar {
    pub name: String,
    pub value: String,
    pub scope: VarScope,
}

/// One `{{name}}` seen in a preview: resolved or not. `value`/`scope` are
/// `None` when nothing answered the name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VarPreview {
    pub name: String,
    pub value: Option<String>,
    pub scope: Option<VarScope>,
}

/// The rendered text plus a row per variable occurrence. Unresolved names stay
/// literal in `text`, unlike `resolve()` which errors.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Preview {
    pub text: String,
    pub vars: Vec<VarPreview>,
}

impl Preview {
    pub fn has_unresolved(&self) -> bool {
        self.vars.iter().any(|var| var.value.is_none())
    }

    /// The names still lacking a value, in the order they appear.
    pub fn unresolved(&self) -> Vec<&str> {
        self.vars
            .iter()
            .filter(|var| var.value.is_none())
            .map(|var| var.name.as_str())
            .collect()
    }
}

/// Variables grouped into ordered layers. A name is answered by the
/// highest-precedence layer that defines it.
#[derive(Debug, Clone, Default)]
pub struct ScopedVars {
    layers: HashMap<VarScope, HashMap<String, String>>,
}

impl ScopedVars {
    pub fn new() -> Self {
        Self::default()
    }

    /// Replace one scope's variables. Disabled rows and rows with an empty key
    /// are ignored, matching [`crate::template`].
    pub fn set_scope(&mut self, scope: VarScope, vars: impl IntoIterator<Item = Kv>) {
        let layer = vars
            .into_iter()
            .filter(|kv| kv.enabled && !kv.key.is_empty())
            .map(|kv| (kv.key, kv.value))
            .collect();
        self.layers.insert(scope, layer);
    }

    pub fn clear_scope(&mut self, scope: VarScope) {
        self.layers.remove(&scope);
    }

    /// The winning value for `name`, or `None` when no scope defines it.
    pub fn lookup(&self, name: &str) -> Option<ResolvedVar> {
        PRECEDENCE.iter().rev().find_map(|&scope| {
            self.layers
                .get(&scope)
                .and_then(|layer| layer.get(name))
                .map(|value| ResolvedVar {
                    name: name.to_owned(),
                    value: value.clone(),
                    scope,
                })
        })
    }

    /// Every distinct name across all scopes, once: the winning value and the
    /// scope that supplies it. Narrowest scope first, then alphabetical, so a
    /// completion list shows the layers that win before the ones they shadow.
    ///
    /// Masking is the caller's business — a secret's value is still here.
    pub fn names(&self) -> Vec<ResolvedVar> {
        let mut resolved: Vec<ResolvedVar> = Vec::new();

        // Narrowest first, so a name met early is the one that wins and the
        // wider scopes that also define it are skipped.
        for scope in PRECEDENCE.iter().rev() {
            let Some(layer) = self.layers.get(scope) else {
                continue;
            };
            for (name, value) in layer {
                if resolved.iter().any(|each| &each.name == name) {
                    continue;
                }
                resolved.push(ResolvedVar {
                    name: name.clone(),
                    value: value.clone(),
                    scope: *scope,
                });
            }
        }

        // Collection order is already by precedence, and this sort is cached and
        // stable, so each scope stays a block.
        resolved.sort_by_cached_key(|each| {
            (
                precedence_rank(each.scope),
                each.name.to_lowercase(),
                each.name.clone(),
            )
        });
        resolved
    }

    /// A [`VarContext`] for `prepare()`, scopes pushed lowest-first so the same
    /// name resolves to the same value as [`Self::lookup`].
    pub fn context(&self) -> VarContext {
        let mut context = VarContext::new();
        for scope in PRECEDENCE {
            if let Some(layer) = self.layers.get(&scope) {
                context.push_scope(
                    layer
                        .iter()
                        .map(|(key, value)| (key.clone(), value.clone())),
                );
            }
        }
        context
    }

    /// Render `input` non-strictly: known names are substituted and recorded,
    /// unknown names stay literal and are reported as unresolved.
    pub fn preview(&self, input: &str) -> Preview {
        let mut preview = Preview::default();
        let mut rest = input;

        while let Some(start) = rest.find("{{") {
            preview.text.push_str(&rest[..start]);

            let after = &rest[start + 2..];
            let Some(end) = after.find("}}") else {
                // Unterminated braces are literal text, not a variable.
                preview.text.push_str(&rest[start..]);
                return preview;
            };

            let token_end = start + 2 + end + 2;
            let name = after[..end].trim();

            match self.lookup(name) {
                Some(resolved) => {
                    preview.text.push_str(&resolved.value);
                    preview.vars.push(VarPreview {
                        name: resolved.name,
                        value: Some(resolved.value),
                        scope: Some(resolved.scope),
                    });
                }
                // Empty or unknown: keep the braces and flag the name.
                None => {
                    preview.text.push_str(&rest[start..token_end]);
                    preview.vars.push(VarPreview {
                        name: name.to_owned(),
                        value: None,
                        scope: None,
                    });
                }
            }

            rest = &rest[token_end..];
        }

        preview.text.push_str(rest);
        preview
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Method;
    use crate::template::resolve;

    fn var(key: &str, value: &str) -> Kv {
        Kv::new(key, value)
    }

    #[test]
    fn later_scope_wins() {
        let mut vars = ScopedVars::new();
        vars.set_scope(VarScope::Global, [var("host", "global")]);
        vars.set_scope(VarScope::Environment, [var("host", "env")]);

        let resolved = vars.lookup("host").unwrap();
        assert_eq!(resolved.value, "env");
        assert_eq!(resolved.scope, VarScope::Environment);
    }

    #[test]
    fn context_agrees_with_lookup() {
        let mut vars = ScopedVars::new();
        vars.set_scope(VarScope::Global, [var("host", "global"), var("only", "g")]);
        vars.set_scope(VarScope::Request, [var("host", "request")]);

        let context = vars.context();
        assert_eq!(context.get("host"), Some("request"));
        assert_eq!(
            context.get("host"),
            vars.lookup("host").map(|r| r.value).as_deref()
        );
        assert_eq!(context.get("only"), Some("g"));
        assert_eq!(resolve("{{host}}", &context).unwrap(), "request");
    }

    #[test]
    fn disabled_rows_are_ignored() {
        let mut vars = ScopedVars::new();
        let mut disabled = var("host", "ignored");
        disabled.enabled = false;
        vars.set_scope(VarScope::Global, [disabled, var("", "blank-key")]);

        assert!(vars.lookup("host").is_none());
        assert!(vars.lookup("").is_none());
    }

    #[test]
    fn names_reports_each_winner_once_grouped_by_precedence() {
        let mut vars = ScopedVars::new();
        vars.set_scope(VarScope::Global, [var("zulu", "g"), var("base", "g")]);
        vars.set_scope(VarScope::Environment, [var("base", "e"), var("alpha", "e")]);
        vars.set_scope(VarScope::Live, [var("live", "l")]);

        let names: Vec<(String, VarScope)> = vars
            .names()
            .into_iter()
            .map(|each| (each.name, each.scope))
            .collect();

        // `base` is defined twice and listed once, in the scope that wins.
        assert_eq!(
            names,
            vec![
                ("live".to_owned(), VarScope::Live),
                ("alpha".to_owned(), VarScope::Environment),
                ("base".to_owned(), VarScope::Environment),
                ("zulu".to_owned(), VarScope::Global),
            ]
        );
        assert_eq!(
            vars.names()
                .iter()
                .find(|each| each.name == "base")
                .map(|each| each.value.as_str()),
            Some("e")
        );
    }

    #[test]
    fn names_agrees_with_lookup() {
        let mut vars = ScopedVars::new();
        vars.set_scope(VarScope::Global, [var("a", "1")]);
        vars.set_scope(VarScope::Collection, [var("a", "2"), var("b", "3")]);

        let names = vars.names();
        assert_eq!(names.len(), 2);
        for row in names {
            assert_eq!(vars.lookup(&row.name), Some(row));
        }
    }

    #[test]
    fn no_scopes_means_no_names() {
        assert!(ScopedVars::new().names().is_empty());
    }

    #[test]
    fn preview_marks_unresolved_and_keeps_the_literal() {
        let mut vars = ScopedVars::new();
        vars.set_scope(VarScope::Global, [var("known", "value")]);

        let preview = vars.preview("a {{known}} b {{missing}} c");
        assert_eq!(preview.text, "a value b {{missing}} c");
        assert!(preview.has_unresolved());
        assert_eq!(preview.unresolved(), vec!["missing"]);
        assert_eq!(preview.vars[0].value.as_deref(), Some("value"));
        assert_eq!(preview.vars[0].scope, Some(VarScope::Global));
        assert_eq!(preview.vars[1].value, None);
    }

    #[test]
    fn preview_leaves_unterminated_and_empty_braces_alone() {
        let vars = ScopedVars::new();

        let unterminated = vars.preview("{{ unterminated");
        assert_eq!(unterminated.text, "{{ unterminated");
        assert!(unterminated.vars.is_empty());

        // `{{}}` has no name: unresolved, and left literal not dropped.
        let empty = vars.preview("{{}}");
        assert_eq!(empty.text, "{{}}");
        assert!(empty.has_unresolved());
        assert_eq!(empty.unresolved(), vec![""]);
    }

    #[test]
    fn request_def_is_not_needed_to_preview_a_url() {
        // Guards the non-strict contract used by B10: an unknown name must not
        // prevent the rest of the request from rendering.
        let mut vars = ScopedVars::new();
        vars.set_scope(
            VarScope::Environment,
            [var("path", "/v1"), var("token", "abc")],
        );

        let request = crate::model::RequestDef::new(Method::Get, "https://api{{path}}?t={{token}}");
        let preview = vars.preview(&request.url);
        assert!(!preview.has_unresolved());
        assert_eq!(preview.text, "https://api/v1?t=abc");
    }
}
