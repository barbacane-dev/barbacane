//! Command names for the control-plane API's operations.
//!
//! An operation's command is `<resource> <name>`: the resource is its tag in
//! kebab case (`ProjectPlugins` → `project-plugins`), the name its
//! `operationId` without the words that repeat the resource
//! (`listProjectPlugins` → `list`). Shared by the build script, which
//! generates the commands, and the crate's tests.

/// The lowercase words of a `camelCase` or `PascalCase` identifier.
pub fn words(identifier: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut current = String::new();
    for c in identifier.chars() {
        if c.is_ascii_uppercase() && !current.is_empty() {
            words.push(std::mem::take(&mut current));
        }
        if c.is_ascii_alphanumeric() {
            current.push(c.to_ascii_lowercase());
        }
    }
    if !current.is_empty() {
        words.push(current);
    }
    words
}

/// The resource a tag names, in kebab case.
pub fn resource(tag: &str) -> String {
    words(tag).join("-")
}

/// A flag for a parameter or field name: `projectId` → `project-id`,
/// `config_schema` → `config-schema`.
pub fn flag(name: &str) -> String {
    name.split('_')
        .flat_map(words)
        .collect::<Vec<_>>()
        .join("-")
}

/// The words of an operation's command name: the verb that starts its
/// `operationId`, then what follows it, without the resource's words
/// (plural or singular).
pub fn name_words(tag: &str, operation_id: &str) -> Vec<String> {
    let mut all = words(operation_id);
    if all.is_empty() {
        return all;
    }
    let verb = all.remove(0);
    let resource = words(tag);
    let mut singular = resource.clone();
    if let Some(last) = singular.last_mut() {
        if let Some(stem) = last.strip_suffix('s') {
            *last = stem.to_string();
        }
    }
    for candidate in [&resource, &singular] {
        let n = candidate.len();
        if n == 0 || all.len() < n {
            continue;
        }
        if let Some(at) = (0..=all.len() - n).find(|&i| all[i..i + n] == candidate[..]) {
            all.drain(at..at + n);
            break;
        }
    }
    let mut name = vec![verb];
    name.extend(all);
    name
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identifiers_split_into_words() {
        assert_eq!(
            words("listProjectApiKeys"),
            ["list", "project", "api", "keys"]
        );
        assert_eq!(words("ProjectPlugins"), ["project", "plugins"]);
    }

    #[test]
    fn a_name_drops_the_resource_it_repeats() {
        assert_eq!(name_words("Projects", "listProjects"), ["list"]);
        assert_eq!(name_words("Projects", "getProject"), ["get"]);
        assert_eq!(name_words("ProjectPlugins", "addProjectPlugin"), ["add"]);
        assert_eq!(
            name_words("ApiKeys", "revokeProjectApiKey"),
            ["revoke", "project"]
        );
        assert_eq!(
            name_words("Plugins", "listPluginVersions"),
            ["list", "versions"]
        );
        assert_eq!(name_words("Specs", "listProjectSpecs"), ["list", "project"]);
    }

    #[test]
    fn flags_are_kebab_case() {
        assert_eq!(flag("projectId"), "project-id");
        assert_eq!(flag("config_schema"), "config-schema");
        assert_eq!(flag("id"), "id");
        assert_eq!(resource("DataPlanes"), "data-planes");
    }
}
