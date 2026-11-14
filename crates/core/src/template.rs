//! `{{variable}}` interpolation.
//!
//! Scopes are searched last-to-first, so a later pushed scope wins. The app
//! pushes globals, then the collection, then the environment, then per-request
//! overrides — in that order.

use std::collections::HashMap;

use crate::error::{Error, Result};

#[derive(Debug, Clone, Default)]
pub struct VarContext {
    scopes: Vec<HashMap<String, String>>,
}

impl VarContext {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_scope<I, K, V>(mut self, vars: I) -> Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<String>,
        V: Into<String>,
    {
        self.push_scope(vars);
        self
    }

    pub fn push_scope<I, K, V>(&mut self, vars: I)
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<String>,
        V: Into<String>,
    {
        let scope = vars
            .into_iter()
            .map(|(key, value)| (key.into(), value.into()))
            .collect();
        self.scopes.push(scope);
    }

    pub fn get(&self, name: &str) -> Option<&str> {
        self.scopes
            .iter()
            .rev()
            .find_map(|scope| scope.get(name).map(String::as_str))
    }

    pub fn is_empty(&self) -> bool {
        self.scopes.iter().all(HashMap::is_empty)
    }
}

/// Replaces every `{{name}}` in `input`. Unknown variables are an error rather
/// than an empty string: a silently blank URL or credential is worse than a
/// visible failure.
pub fn resolve(input: &str, ctx: &VarContext) -> Result<String> {
    let mut out = String::with_capacity(input.len());
    let mut rest = input;

    while let Some(start) = rest.find("{{") {
        out.push_str(&rest[..start]);

        let after = &rest[start + 2..];
        let Some(end) = after.find("}}") else {
            // Unterminated braces are literal text, not a variable.
            out.push_str(&rest[start..]);
            return Ok(out);
        };

        let name = after[..end].trim();
        if name.is_empty() {
            return Err(Error::UnknownVariable(String::new()));
        }

        match ctx.get(name) {
            Some(value) => out.push_str(value),
            None => return Err(Error::UnknownVariable(name.to_owned())),
        }

        rest = &after[end + 2..];
    }

    out.push_str(rest);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> VarContext {
        VarContext::new()
            .with_scope([("host", "api.example.com")])
            .with_scope([("token", "abc123")])
    }

    #[test]
    fn resolves_known_variables() {
        let resolved = resolve("https://{{host}}/v1?t={{ token }}", &ctx()).unwrap();
        assert_eq!(resolved, "https://api.example.com/v1?t=abc123");
    }

    #[test]
    fn later_scope_wins() {
        let mut context = VarContext::new();
        context.push_scope([("host", "first")]);
        context.push_scope([("host", "second")]);
        assert_eq!(resolve("{{host}}", &context).unwrap(), "second");
    }

    #[test]
    fn unknown_variable_is_an_error() {
        let error = resolve("{{missing}}", &ctx()).unwrap_err();
        assert!(matches!(error, Error::UnknownVariable(name) if name == "missing"));
    }

    #[test]
    fn text_without_variables_is_untouched() {
        assert_eq!(resolve("plain text", &ctx()).unwrap(), "plain text");
        assert_eq!(resolve("{{ unterminated", &ctx()).unwrap(), "{{ unterminated");
    }
}
