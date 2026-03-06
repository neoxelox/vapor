use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use globset::{GlobBuilder, GlobMatcher};

use crate::logging;

const VAPOR_IGNORE_FILE_NAME: &str = ".vaporignore";
const GIT_IGNORE_FILE_NAME: &str = ".gitignore";
const VAPOR_USE_GITIGNORE_ENV_KEY: &str = "VAPOR_USE_GITIGNORE";

const DEFAULT_IGNORE_RULES: &[&str] = &[
    ".git/",
    ".DS_Store",
    "*.tmp",
    "*.temp",
    "*.swp",
    "*.swo",
    "*~",
    "node_modules/",
    ".pnpm-store/",
    ".yarn/cache/",
    ".yarn/unplugged/",
    ".npm/",
    ".next/",
    ".nuxt/",
    ".svelte-kit/",
    "dist/",
    "build/",
    "out/",
    ".turbo/",
    ".vite/",
    ".parcel-cache/",
    "coverage/",
    "storybook-static/",
    "*.tsbuildinfo",
    ".eslintcache",
    "*.log",
    ".env.local",
];

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EventPathFilterOptions {
    pub use_gitignore: bool,
    pub user_rules: Vec<String>,
}

impl Default for EventPathFilterOptions {
    fn default() -> Self {
        Self {
            use_gitignore: true,
            user_rules: Vec::new(),
        }
    }
}

impl EventPathFilterOptions {
    pub fn from_process_environment() -> Self {
        let use_gitignore =
            resolve_use_gitignore(env::var(VAPOR_USE_GITIGNORE_ENV_KEY).ok().as_deref());

        Self {
            use_gitignore,
            ..Self::default()
        }
    }
}

fn resolve_use_gitignore(value: Option<&str>) -> bool {
    value.and_then(parse_bool_flag).unwrap_or(true)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RuleAction {
    Ignore,
    Allow,
}

#[derive(Debug)]
struct CompiledRule {
    action: RuleAction,
    matchers: Vec<GlobMatcher>,
}

impl CompiledRule {
    fn matches(&self, relative_path: &str) -> bool {
        self.matchers
            .iter()
            .any(|matcher| matcher.is_match(relative_path))
    }
}

#[derive(Debug)]
pub struct EventPathFilter {
    watch_root: PathBuf,
    rules: Vec<CompiledRule>,
}

impl EventPathFilter {
    pub fn for_watch_root(watch_root: &Path, options: &EventPathFilterOptions) -> Self {
        let mut rules = Vec::new();

        append_default_rules(&mut rules);

        if options.use_gitignore {
            append_rules_from_file(&mut rules, watch_root.join(GIT_IGNORE_FILE_NAME));
        }

        append_rules_from_file(&mut rules, watch_root.join(VAPOR_IGNORE_FILE_NAME));
        append_user_rules(&mut rules, &options.user_rules);

        logging::info(
            "Initialized filesystem path filter",
            &[
                ("watch_root", watch_root.display().to_string()),
                ("rule_count", rules.len().to_string()),
                ("use_gitignore", options.use_gitignore.to_string()),
            ],
        );

        Self {
            watch_root: watch_root.to_path_buf(),
            rules,
        }
    }

    pub fn should_ignore(&self, path: &Path) -> bool {
        let Ok(relative_path) = path.strip_prefix(&self.watch_root) else {
            return false;
        };

        let normalized_relative_path = normalize_relative_path(relative_path);
        if normalized_relative_path.is_empty() {
            return false;
        }

        let mut ignored = false;
        for rule in &self.rules {
            if rule.matches(&normalized_relative_path) {
                ignored = matches!(rule.action, RuleAction::Ignore);
            }
        }

        ignored
    }
}

fn append_default_rules(rules: &mut Vec<CompiledRule>) {
    for rule in DEFAULT_IGNORE_RULES {
        append_rule_line(rules, rule, "defaults", None, None);
    }
}

fn append_rules_from_file(rules: &mut Vec<CompiledRule>, path: PathBuf) {
    if !path.exists() {
        return;
    }

    let contents = match fs::read_to_string(&path) {
        Ok(contents) => contents,
        Err(error) => {
            logging::warning(
                "Failed to read ignore rules file; skipping",
                &[
                    ("path", path.display().to_string()),
                    ("error", error.to_string()),
                ],
            );
            return;
        }
    };

    for (index, line) in contents.lines().enumerate() {
        append_rule_line(rules, line, "file", Some(path.as_path()), Some(index + 1));
    }
}

fn append_user_rules(rules: &mut Vec<CompiledRule>, user_rules: &[String]) {
    for rule in user_rules {
        append_rule_line(rules, rule, "user", None, None);
    }
}

fn append_rule_line(
    rules: &mut Vec<CompiledRule>,
    raw_line: &str,
    source: &str,
    source_path: Option<&Path>,
    line_number: Option<usize>,
) {
    let Some((action, pattern)) = parse_rule_line(raw_line) else {
        return;
    };

    match compile_rule(action, pattern) {
        Ok(rule) => rules.push(rule),
        Err(error) => {
            let mut metadata = vec![
                ("source", source.to_string()),
                ("pattern", pattern.to_string()),
                ("error", error),
            ];
            if let Some(path) = source_path {
                metadata.push(("path", path.display().to_string()));
            }
            if let Some(line_number) = line_number {
                metadata.push(("line", line_number.to_string()));
            }

            logging::warning("Skipping invalid ignore rule", &metadata);
        }
    }
}

fn parse_rule_line(raw_line: &str) -> Option<(RuleAction, &str)> {
    let trimmed = raw_line.trim();
    if trimmed.is_empty() || trimmed.starts_with('#') {
        return None;
    }

    if let Some(unignore_pattern) = trimmed.strip_prefix('!') {
        let unignore_pattern = unignore_pattern.trim();
        if unignore_pattern.is_empty() {
            return None;
        }
        return Some((RuleAction::Allow, unignore_pattern));
    }

    Some((RuleAction::Ignore, trimmed))
}

fn compile_rule(action: RuleAction, raw_pattern: &str) -> Result<CompiledRule, String> {
    let patterns = expand_glob_patterns(raw_pattern)?;
    let mut matchers = Vec::with_capacity(patterns.len());

    for pattern in patterns {
        let matcher = GlobBuilder::new(&pattern)
            .literal_separator(true)
            .backslash_escape(true)
            .build()
            .map_err(|error| error.to_string())?
            .compile_matcher();
        matchers.push(matcher);
    }

    Ok(CompiledRule { action, matchers })
}

fn expand_glob_patterns(raw_pattern: &str) -> Result<Vec<String>, String> {
    let anchored = raw_pattern.starts_with('/');
    let directory_only = raw_pattern.ends_with('/');

    let mut body = raw_pattern;
    if anchored {
        body = body.trim_start_matches('/');
    }
    if directory_only {
        body = body.trim_end_matches('/');
    }

    if body.is_empty() {
        return Err("ignore pattern is empty".to_string());
    }

    let mut stems = if anchored {
        vec![body.to_string()]
    } else {
        vec![body.to_string(), format!("**/{body}")]
    };
    stems.sort();
    stems.dedup();

    let mut patterns = Vec::new();
    for stem in stems {
        patterns.push(stem.clone());
        if directory_only {
            patterns.push(format!("{stem}/**"));
        }
    }

    patterns.sort();
    patterns.dedup();
    Ok(patterns)
}

fn normalize_relative_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

fn parse_bool_flag(value: &str) -> Option<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn defaults_ignore_common_cache_and_build_paths() {
        let watch_root = create_test_directory();
        let filter =
            EventPathFilter::for_watch_root(&watch_root, &EventPathFilterOptions::default());

        assert!(filter.should_ignore(&watch_root.join(".git/config")));
        assert!(filter.should_ignore(&watch_root.join("node_modules/pkg/index.js")));
        assert!(filter.should_ignore(&watch_root.join("coverage/unit.json")));
        assert!(!filter.should_ignore(&watch_root.join("src/main.rs")));

        remove_test_directory(&watch_root);
    }

    #[test]
    fn gitignore_rules_can_be_disabled() {
        let watch_root = create_test_directory();
        fs::write(watch_root.join(".gitignore"), "generated/\n")
            .expect("failed to write .gitignore");

        let filter_with_gitignore =
            EventPathFilter::for_watch_root(&watch_root, &EventPathFilterOptions::default());
        assert!(filter_with_gitignore.should_ignore(&watch_root.join("generated/file.txt")));

        let filter_without_gitignore = EventPathFilter::for_watch_root(
            &watch_root,
            &EventPathFilterOptions {
                use_gitignore: false,
                user_rules: Vec::new(),
            },
        );
        assert!(!filter_without_gitignore.should_ignore(&watch_root.join("generated/file.txt")));

        remove_test_directory(&watch_root);
    }

    #[test]
    fn vaporignore_and_user_rules_override_defaults_and_gitignore() {
        let watch_root = create_test_directory();

        fs::write(
            watch_root.join(".gitignore"),
            "!coverage/from-gitignore.txt\n",
        )
        .expect("failed to write .gitignore");
        fs::write(
            watch_root.join(".vaporignore"),
            "coverage/\n!coverage/from-vaporignore.txt\n",
        )
        .expect("failed to write .vaporignore");

        let filter = EventPathFilter::for_watch_root(
            &watch_root,
            &EventPathFilterOptions {
                use_gitignore: true,
                user_rules: vec![
                    "!coverage/from-user-rule.txt".to_string(),
                    "coverage/from-vaporignore.txt".to_string(),
                ],
            },
        );

        assert!(filter.should_ignore(&watch_root.join("coverage/regular.txt")));
        assert!(filter.should_ignore(&watch_root.join("coverage/from-gitignore.txt")));
        assert!(filter.should_ignore(&watch_root.join("coverage/from-vaporignore.txt")));
        assert!(!filter.should_ignore(&watch_root.join("coverage/from-user-rule.txt")));

        remove_test_directory(&watch_root);
    }

    #[test]
    fn resolves_use_gitignore_with_safe_default() {
        assert!(resolve_use_gitignore(None));
    }

    #[test]
    fn resolves_use_gitignore_from_explicit_values() {
        assert!(resolve_use_gitignore(Some("true")));
        assert!(!resolve_use_gitignore(Some("false")));
        assert!(resolve_use_gitignore(Some("invalid")));
    }

    #[test]
    fn parses_boolean_environment_flags() {
        assert_eq!(parse_bool_flag("true"), Some(true));
        assert_eq!(parse_bool_flag("YES"), Some(true));
        assert_eq!(parse_bool_flag("0"), Some(false));
        assert_eq!(parse_bool_flag("off"), Some(false));
        assert_eq!(parse_bool_flag("invalid"), None);
    }

    fn create_test_directory() -> PathBuf {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock drift")
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "vapor-daemon-path-filter-{}-{}",
            std::process::id(),
            timestamp
        ));
        fs::create_dir_all(&root).expect("failed to create test directory");
        root
    }

    fn remove_test_directory(path: &Path) {
        let _ = fs::remove_dir_all(path);
    }
}
