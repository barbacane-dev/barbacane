use std::collections::HashMap;

/// The routing trie. Maps HTTP paths + methods to route matches.
#[derive(Debug, Default)]
pub struct Router {
    root: Node,
}

/// A single node in the prefix trie.
#[derive(Debug, Default)]
struct Node {
    /// Static children keyed by segment name.
    static_children: HashMap<String, Node>,
    /// Parameter child (at most one per node).
    param_child: Option<Box<ParamNode>>,
    /// Method-to-route mapping at this terminal node.
    methods: HashMap<String, RouteEntry>,
}

/// A parameter segment node.
#[derive(Debug)]
struct ParamNode {
    /// Parameter name (e.g. "id").
    name: String,
    /// The subtree below this parameter.
    node: Node,
}

/// A matched route entry.
#[derive(Debug, Clone)]
pub struct RouteEntry {
    /// Index into the compiled operations list.
    pub operation_index: usize,
}

/// The result of a route lookup.
#[derive(Debug)]
pub enum RouteMatch {
    /// Matched a path and method.
    Found {
        entry: RouteEntry,
        params: Vec<(String, String)>,
    },
    /// Path matched but method is not allowed.
    MethodNotAllowed { allowed: Vec<String> },
    /// No path matched.
    NotFound,
}

impl Router {
    /// Create a new empty router.
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert a route into the trie.
    pub fn insert(
        &mut self,
        _path: &str,
        _method: &str,
        _entry: RouteEntry,
    ) {
        todo!("M1: implement trie insertion")
    }

    /// Look up a request path and method.
    pub fn lookup(&self, _path: &str, _method: &str) -> RouteMatch {
        todo!("M1: implement trie lookup")
    }
}

/// Normalize a request path: strip trailing slashes, collapse double slashes.
pub fn normalize_path(path: &str) -> String {
    let mut normalized = String::with_capacity(path.len());
    let mut prev_slash = false;

    for ch in path.chars() {
        if ch == '/' {
            if !prev_slash {
                normalized.push('/');
            }
            prev_slash = true;
        } else {
            normalized.push(ch);
            prev_slash = false;
        }
    }

    // Strip trailing slash (but keep root "/")
    if normalized.len() > 1 && normalized.ends_with('/') {
        normalized.pop();
    }

    if normalized.is_empty() {
        "/".to_string()
    } else {
        normalized
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_strips_trailing_slash() {
        assert_eq!(normalize_path("/users/"), "/users");
    }

    #[test]
    fn normalize_collapses_double_slashes() {
        assert_eq!(normalize_path("/users//123"), "/users/123");
    }

    #[test]
    fn normalize_preserves_root() {
        assert_eq!(normalize_path("/"), "/");
    }

    #[test]
    fn normalize_combined() {
        assert_eq!(normalize_path("/users//123//orders/"), "/users/123/orders");
    }
}
