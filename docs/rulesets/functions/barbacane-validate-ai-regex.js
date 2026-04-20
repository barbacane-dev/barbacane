// Validates regex patterns inside AI middleware configs at lint time so
// operators catch invalid patterns in CI rather than from a 500 on the
// first production request. Runs per-middleware; expects a single
// `x-barbacane-middlewares` entry as input.
//
// Covered fields:
// - ai-prompt-guard:    profiles.*.blocked_patterns[]
// - ai-response-guard:  profiles.*.redact[].pattern  + profiles.*.blocked_patterns[]
//
// Rust `regex` crate syntax is a subset of PCRE close enough to JavaScript
// for this purpose: the common mistakes (unclosed brackets, stray
// quantifiers, invalid character classes) parse the same. Rust-specific
// inline flags (`(?-u)`, `(?x)`) are tolerated — if JS can't parse them
// we skip the pattern rather than false-positive.

function getSchema() {
  return {
    name: "barbacane-validate-ai-regex",
    description:
      "Compile-checks regex patterns in ai-prompt-guard and ai-response-guard profiles",
  };
}

function tryCompile(pattern) {
  // Rust-specific inline flags JS won't accept — skip, let runtime decide.
  if (/^\(\?[\w-]+\)/.test(pattern)) {
    // Leading (?flags) — check the remainder.
    try {
      new RegExp(pattern.replace(/^\(\?[\w-]+\)/, ""));
      return null;
    } catch (_) {
      // Even with flags stripped it's broken — report it.
    }
  }
  try {
    new RegExp(pattern);
    return null;
  } catch (e) {
    return String(e && e.message ? e.message : e);
  }
}

function collectPatterns(middleware) {
  const list = [];
  const cfg = middleware && middleware.config;
  if (!cfg || typeof cfg !== "object") return list;

  const profiles = cfg.profiles;
  if (!profiles || typeof profiles !== "object") return list;

  for (const [profileName, profile] of Object.entries(profiles)) {
    if (!profile || typeof profile !== "object") continue;

    // ai-prompt-guard.profiles.*.blocked_patterns — array of strings
    if (Array.isArray(profile.blocked_patterns)) {
      profile.blocked_patterns.forEach((p, idx) => {
        if (typeof p === "string") {
          list.push({
            pattern: p,
            path: `profiles.${profileName}.blocked_patterns[${idx}]`,
          });
        }
      });
    }

    // ai-response-guard.profiles.*.redact[].pattern — array of {pattern, replacement}
    if (Array.isArray(profile.redact)) {
      profile.redact.forEach((rule, idx) => {
        if (rule && typeof rule.pattern === "string") {
          list.push({
            pattern: rule.pattern,
            path: `profiles.${profileName}.redact[${idx}].pattern`,
          });
        }
      });
    }
  }

  return list;
}

function runRule(input) {
  const results = [];
  if (!input || typeof input !== "object") return results;

  const name = input.name;
  if (name !== "ai-prompt-guard" && name !== "ai-response-guard") return results;

  for (const { pattern, path } of collectPatterns(input)) {
    const err = tryCompile(pattern);
    if (err) {
      results.push({
        message: `Invalid regex in ${name} ${path}: "${pattern}" — ${err}`,
      });
    }
  }

  return results;
}
