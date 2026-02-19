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
    /// Wildcard child — matches all remaining path segments joined by `/`.
    /// Only valid at a terminal position (no further trie nodes after it).
    wildcard_child: Option<Box<WildcardNode>>,
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

/// A wildcard segment node (e.g. `{path+}`).
///
/// Matches all remaining path segments joined by `/` and captures the result
/// as a single parameter value.
#[derive(Debug)]
struct WildcardNode {
    /// Parameter name (e.g. "path" from `{path+}`).
    name: String,
    /// Terminal node — holds the method map.
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

/// A parsed path segment.
#[derive(Debug, Clone)]
enum Segment {
    Static(String),
    Param(String),
    /// Matches all remaining segments joined by `/`. Must be the last segment.
    Wildcard(String),
}

impl Router {
    /// Create a new empty router.
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert a route into the trie.
    ///
    /// Path should be a template like "/users/{id}/orders".
    /// Method should be uppercase (e.g. "GET", "POST").
    pub fn insert(&mut self, path: &str, method: &str, entry: RouteEntry) {
        let segments = parse_path_template(path);
        let node = self.traverse_or_create(&segments);
        node.methods.insert(method.to_uppercase(), entry);
    }

    /// Look up a request path and method.
    ///
    /// Path should be an actual request path (not a template).
    /// Method should be uppercase.
    pub fn lookup(&self, path: &str, method: &str) -> RouteMatch {
        let normalized = normalize_path(path);
        let segments: Vec<&str> = normalized.split('/').filter(|s| !s.is_empty()).collect();

        let mut params = Vec::new();
        match self.traverse_and_match(&self.root, &segments, &mut params) {
            Some(node) => {
                if let Some(entry) = node.methods.get(&method.to_uppercase()) {
                    RouteMatch::Found {
                        entry: entry.clone(),
                        params,
                    }
                } else if node.methods.is_empty() {
                    RouteMatch::NotFound
                } else {
                    let mut allowed: Vec<String> = node.methods.keys().cloned().collect();
                    allowed.sort();
                    RouteMatch::MethodNotAllowed { allowed }
                }
            }
            None => RouteMatch::NotFound,
        }
    }

    /// Traverse or create nodes for a path template.
    fn traverse_or_create(&mut self, segments: &[Segment]) -> &mut Node {
        let mut current = &mut self.root;

        for segment in segments {
            current = match segment {
                Segment::Static(name) => current.static_children.entry(name.clone()).or_default(),
                Segment::Param(name) => {
                    if current.param_child.is_none() {
                        current.param_child = Some(Box::new(ParamNode {
                            name: name.clone(),
                            node: Node::default(),
                        }));
                    }
                    &mut current.param_child.as_mut().expect("just set above").node
                }
                Segment::Wildcard(name) => {
                    if current.wildcard_child.is_none() {
                        current.wildcard_child = Some(Box::new(WildcardNode {
                            name: name.clone(),
                            node: Node::default(),
                        }));
                    }
                    &mut current
                        .wildcard_child
                        .as_mut()
                        .expect("just set above")
                        .node
                }
            };
        }

        current
    }

    /// Traverse the trie matching actual path segments, capturing parameters.
    /// Returns the terminal node if the path matches, None otherwise.
    fn traverse_and_match<'a>(
        &'a self,
        node: &'a Node,
        segments: &[&str],
        params: &mut Vec<(String, String)>,
    ) -> Option<&'a Node> {
        if segments.is_empty() {
            return Some(node);
        }

        let segment = segments[0];
        let remaining = &segments[1..];

        // Static children take precedence (most specific match).
        if let Some(child) = node.static_children.get(segment) {
            if let Some(result) = self.traverse_and_match(child, remaining, params) {
                return Some(result);
            }
        }

        // Try single-segment parameter child.
        if let Some(param_child) = &node.param_child {
            let param_len = params.len();
            params.push((param_child.name.clone(), segment.to_string()));

            if let Some(result) = self.traverse_and_match(&param_child.node, remaining, params) {
                return Some(result);
            }

            // Backtrack if this path didn't work.
            params.truncate(param_len);
        }

        // Try wildcard child — consumes all remaining segments (including the current one).
        if let Some(wildcard_child) = &node.wildcard_child {
            let joined = std::iter::once(segment)
                .chain(remaining.iter().copied())
                .collect::<Vec<_>>()
                .join("/");
            let param_len = params.len();
            params.push((wildcard_child.name.clone(), joined));

            if let Some(result) = self.traverse_and_match(&wildcard_child.node, &[], params) {
                return Some(result);
            }

            params.truncate(param_len);
        }

        None
    }
}

/// Parse a path template into segments.
fn parse_path_template(path: &str) -> Vec<Segment> {
    path.split('/')
        .filter(|s| !s.is_empty())
        .map(|s| {
            if s.starts_with('{') && s.ends_with('}') {
                let inner = &s[1..s.len() - 1];
                if let Some(base) = inner.strip_suffix('+') {
                    Segment::Wildcard(base.to_string())
                } else {
                    Segment::Param(inner.to_string())
                }
            } else {
                Segment::Static(s.to_string())
            }
        })
        .collect()
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

    // === Normalization tests ===

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

    // === Routing tests ===

    #[test]
    fn route_static_path() {
        let mut router = Router::new();
        router.insert("/health", "GET", RouteEntry { operation_index: 0 });

        match router.lookup("/health", "GET") {
            RouteMatch::Found { entry, params } => {
                assert_eq!(entry.operation_index, 0);
                assert!(params.is_empty());
            }
            _ => panic!("expected Found"),
        }
    }

    #[test]
    fn route_with_parameter() {
        let mut router = Router::new();
        router.insert("/users/{id}", "GET", RouteEntry { operation_index: 0 });

        match router.lookup("/users/123", "GET") {
            RouteMatch::Found { entry, params } => {
                assert_eq!(entry.operation_index, 0);
                assert_eq!(params, vec![("id".to_string(), "123".to_string())]);
            }
            _ => panic!("expected Found"),
        }
    }

    #[test]
    fn route_with_multiple_parameters() {
        let mut router = Router::new();
        router.insert(
            "/users/{userId}/orders/{orderId}",
            "GET",
            RouteEntry { operation_index: 0 },
        );

        match router.lookup("/users/42/orders/99", "GET") {
            RouteMatch::Found { entry, params } => {
                assert_eq!(entry.operation_index, 0);
                assert_eq!(
                    params,
                    vec![
                        ("userId".to_string(), "42".to_string()),
                        ("orderId".to_string(), "99".to_string()),
                    ]
                );
            }
            _ => panic!("expected Found"),
        }
    }

    #[test]
    fn route_not_found() {
        let mut router = Router::new();
        router.insert("/users", "GET", RouteEntry { operation_index: 0 });

        match router.lookup("/posts", "GET") {
            RouteMatch::NotFound => {}
            _ => panic!("expected NotFound"),
        }
    }

    #[test]
    fn route_method_not_allowed() {
        let mut router = Router::new();
        router.insert("/users", "GET", RouteEntry { operation_index: 0 });
        router.insert("/users", "POST", RouteEntry { operation_index: 1 });

        match router.lookup("/users", "DELETE") {
            RouteMatch::MethodNotAllowed { allowed } => {
                assert!(allowed.contains(&"GET".to_string()));
                assert!(allowed.contains(&"POST".to_string()));
            }
            _ => panic!("expected MethodNotAllowed"),
        }
    }

    #[test]
    fn static_takes_precedence_over_param() {
        let mut router = Router::new();
        router.insert("/users/me", "GET", RouteEntry { operation_index: 0 });
        router.insert("/users/{id}", "GET", RouteEntry { operation_index: 1 });

        // "/users/me" should match the static route
        match router.lookup("/users/me", "GET") {
            RouteMatch::Found { entry, params } => {
                assert_eq!(entry.operation_index, 0);
                assert!(params.is_empty());
            }
            _ => panic!("expected Found for static"),
        }

        // "/users/123" should match the param route
        match router.lookup("/users/123", "GET") {
            RouteMatch::Found { entry, params } => {
                assert_eq!(entry.operation_index, 1);
                assert_eq!(params, vec![("id".to_string(), "123".to_string())]);
            }
            _ => panic!("expected Found for param"),
        }
    }

    #[test]
    fn route_root_path() {
        let mut router = Router::new();
        router.insert("/", "GET", RouteEntry { operation_index: 0 });

        match router.lookup("/", "GET") {
            RouteMatch::Found { entry, .. } => {
                assert_eq!(entry.operation_index, 0);
            }
            _ => panic!("expected Found for root"),
        }
    }

    #[test]
    fn route_normalizes_request_path() {
        let mut router = Router::new();
        router.insert("/users/{id}", "GET", RouteEntry { operation_index: 0 });

        // Trailing slash should still match
        match router.lookup("/users/123/", "GET") {
            RouteMatch::Found { params, .. } => {
                assert_eq!(params, vec![("id".to_string(), "123".to_string())]);
            }
            _ => panic!("expected Found"),
        }

        // Double slashes should still match
        match router.lookup("/users//456", "GET") {
            RouteMatch::Found { params, .. } => {
                assert_eq!(params, vec![("id".to_string(), "456".to_string())]);
            }
            _ => panic!("expected Found"),
        }
    }

    #[test]
    fn multiple_methods_same_path() {
        let mut router = Router::new();
        router.insert("/users", "GET", RouteEntry { operation_index: 0 });
        router.insert("/users", "POST", RouteEntry { operation_index: 1 });
        router.insert("/users", "DELETE", RouteEntry { operation_index: 2 });

        match router.lookup("/users", "GET") {
            RouteMatch::Found { entry, .. } => assert_eq!(entry.operation_index, 0),
            _ => panic!("expected Found for GET"),
        }

        match router.lookup("/users", "POST") {
            RouteMatch::Found { entry, .. } => assert_eq!(entry.operation_index, 1),
            _ => panic!("expected Found for POST"),
        }

        match router.lookup("/users", "DELETE") {
            RouteMatch::Found { entry, .. } => assert_eq!(entry.operation_index, 2),
            _ => panic!("expected Found for DELETE"),
        }
    }

    // === Wildcard parameter tests ===

    #[test]
    fn wildcard_matches_single_segment() {
        let mut router = Router::new();
        router.insert("/files/{name+}", "GET", RouteEntry { operation_index: 0 });

        match router.lookup("/files/readme.txt", "GET") {
            RouteMatch::Found { entry, params } => {
                assert_eq!(entry.operation_index, 0);
                assert_eq!(params, vec![("name".to_string(), "readme.txt".to_string())]);
            }
            _ => panic!("expected Found"),
        }
    }

    #[test]
    fn wildcard_matches_multiple_segments() {
        let mut router = Router::new();
        router.insert("/files/{path+}", "GET", RouteEntry { operation_index: 0 });

        match router.lookup("/files/a/b/c/file.txt", "GET") {
            RouteMatch::Found { entry, params } => {
                assert_eq!(entry.operation_index, 0);
                assert_eq!(
                    params,
                    vec![("path".to_string(), "a/b/c/file.txt".to_string())]
                );
            }
            _ => panic!("expected Found"),
        }
    }

    #[test]
    fn wildcard_with_prefix_param() {
        let mut router = Router::new();
        router.insert(
            "/files/{bucket}/{key+}",
            "GET",
            RouteEntry { operation_index: 0 },
        );

        match router.lookup("/files/my-bucket/folder/sub/file.txt", "GET") {
            RouteMatch::Found { entry, params } => {
                assert_eq!(entry.operation_index, 0);
                assert_eq!(
                    params,
                    vec![
                        ("bucket".to_string(), "my-bucket".to_string()),
                        ("key".to_string(), "folder/sub/file.txt".to_string()),
                    ]
                );
            }
            _ => panic!("expected Found"),
        }
    }

    #[test]
    fn static_takes_precedence_over_wildcard() {
        let mut router = Router::new();
        router.insert("/files/special", "GET", RouteEntry { operation_index: 0 });
        router.insert("/files/{path+}", "GET", RouteEntry { operation_index: 1 });

        // Static wins for exact match
        match router.lookup("/files/special", "GET") {
            RouteMatch::Found { entry, params } => {
                assert_eq!(entry.operation_index, 0);
                assert!(params.is_empty());
            }
            _ => panic!("expected Found for static"),
        }

        // Wildcard wins for multi-segment
        match router.lookup("/files/other/file.txt", "GET") {
            RouteMatch::Found { entry, params } => {
                assert_eq!(entry.operation_index, 1);
                assert_eq!(
                    params,
                    vec![("path".to_string(), "other/file.txt".to_string())]
                );
            }
            _ => panic!("expected Found for wildcard"),
        }
    }

    #[test]
    fn param_takes_precedence_over_wildcard() {
        let mut router = Router::new();
        router.insert("/files/{name}", "GET", RouteEntry { operation_index: 0 });
        router.insert("/files/{path+}", "GET", RouteEntry { operation_index: 1 });

        // Single segment: param wins (more specific)
        match router.lookup("/files/readme.txt", "GET") {
            RouteMatch::Found { entry, params } => {
                assert_eq!(entry.operation_index, 0);
                assert_eq!(params, vec![("name".to_string(), "readme.txt".to_string())]);
            }
            _ => panic!("expected Found for param"),
        }

        // Multi-segment: only wildcard can match
        match router.lookup("/files/a/b.txt", "GET") {
            RouteMatch::Found { entry, params } => {
                assert_eq!(entry.operation_index, 1);
                assert_eq!(params, vec![("path".to_string(), "a/b.txt".to_string())]);
            }
            _ => panic!("expected Found for wildcard"),
        }
    }
}
